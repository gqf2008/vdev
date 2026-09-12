//! MSVCRT argv 规则的单参数引号处理（UAC 提权重启时经 CreateProcess 命令行传参用）。
//!
//! Windows 把命令行拼成一个字符串再由目标进程按 MSVCRT / `CommandLineToArgvW`
//! 规则切回 argv：反斜杠只在紧邻引号时才有转义含义——引号前的 n 个反斜杠要写成
//! 2n+1 个（n 个转义自身 + 1 个转义引号），收尾引号前的 n 个反斜杠写成 2n 个
//! （成对转义，避免吃掉收尾引号）。见 Daniel Colascione《Everyone quotes command
//! line arguments the wrong way》与 Raymond Chen 对同一切分规则的说明。
//!
//! 零依赖纯逻辑，可在宿主直接 `rustc --edition 2021 --test` 单测。

/// 把单个参数编码成可被 MSVCRT argv 解析还原的命令行片段。
pub fn quote_arg(arg: &str) -> String {
    // 不含分隔符/引号的非空参数原样即可（此时反斜杠不挨引号，无特殊含义）
    if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
        return arg.to_string();
    }

    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // 引号前的 n 个反斜杠 → 2n+1 个反斜杠 + 引号（引号被转义为字面量）
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // 收尾引号前的 n 个反斜杠 → 2n 个（成对转义，保住收尾引号）
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn plain_args_unchanged() {
        assert_eq!(quote_arg("abc"), "abc");
        assert_eq!(quote_arg("install"), "install");
        assert_eq!(quote_arg("--inf-dir"), "--inf-dir");
        assert_eq!(quote_arg(r"C:\Program.exe"), r"C:\Program.exe");
        assert_eq!(quote_arg("1920x1080@60/120"), "1920x1080@60/120");
    }

    #[test]
    fn empty_arg_becomes_quoted_pair() {
        assert_eq!(quote_arg(""), "\"\"");
    }

    #[test]
    fn spaces_and_tabs_force_quotes() {
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("a\tb"), "\"a\tb\"");
        assert_eq!(quote_arg("C:\\dir name"), "\"C:\\dir name\"");
    }

    #[test]
    fn inner_quotes_are_escaped() {
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
        assert_eq!(
            quote_arg("--name \"vdev 4k\""),
            "\"--name \\\"vdev 4k\\\"\""
        );
    }

    #[test]
    fn backslashes_before_quote_follow_2n1_rule() {
        // 引号前 n 个反斜杠 → 2n+1 个（2n 转义 + 1 转义引号）
        assert_eq!(quote_arg(r"a\b"), r"a\b"); // 不挨引号 → 原样
        assert_eq!(quote_arg("a\\\"b"), "\"a\\\\\\\"b\""); // 1 个 → 3 个
        assert_eq!(quote_arg("a\\\\\"b"), "\"a\\\\\\\\\\\"b\""); // 2 个 → 5 个
    }

    #[test]
    fn trailing_backslashes_double_before_closing_quote() {
        // 收尾引号前 n 个反斜杠 → 2n 个
        assert_eq!(quote_arg("a b\\"), "\"a b\\\\\"");
        assert_eq!(quote_arg("a b\\\\"), "\"a b\\\\\\\\\""); // 2 个 → 4 个
        assert_eq!(quote_arg("C:\\dir name\\"), "\"C:\\dir name\\\\\"");
    }

    /// MSVCRT / CommandLineToArgvW 切分规则的参考实现（测试专用，与 quote_arg
    /// 独立实现），用于验证 quote_arg 的输出能无损还原。
    fn split_cmdline(cmd: &str) -> Vec<String> {
        let chars: Vec<char> = cmd.chars().collect();
        let mut args = Vec::new();
        let mut cur = String::new();
        let mut in_quotes = false;
        let mut has_arg = false;
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\\' => {
                    let mut n = 0;
                    while i < chars.len() && chars[i] == '\\' {
                        n += 1;
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '"' {
                        // 2n 个反斜杠 + 引号 → n 个反斜杠 + 切换引号域；
                        // 2n+1 个 → n 个反斜杠 + 字面引号
                        for _ in 0..n / 2 {
                            cur.push('\\');
                        }
                        if n % 2 == 1 {
                            cur.push('"');
                        } else {
                            in_quotes = !in_quotes;
                        }
                        has_arg = true;
                    } else {
                        for _ in 0..n {
                            cur.push('\\');
                        }
                        has_arg = true;
                    }
                    continue;
                }
                '"' => {
                    in_quotes = !in_quotes;
                    has_arg = true;
                }
                ' ' | '\t' if !in_quotes => {
                    if has_arg {
                        args.push(std::mem::take(&mut cur));
                        has_arg = false;
                    }
                }
                c => {
                    cur.push(c);
                    has_arg = true;
                }
            }
            i += 1;
        }
        if has_arg {
            args.push(cur);
        }
        args
    }

    #[test]
    fn roundtrip_through_reference_parser() {
        let cases: Vec<Vec<String>> = vec![
            vec![
                "install".into(),
                "--inf-dir".into(),
                "C:\\dir with space".into(),
            ],
            vec![
                "add".into(),
                "1920x1080@60".into(),
                "--name".into(),
                "vdev \"4k\"".into(),
            ],
            vec!["add".into(), "--name".into(), "trailing\\".into()],
            vec!["uninstall".into(), "".into()],
            vec!["a\\b".into(), "a\\\\b".into()],
            vec!["tab\targ".into()],
        ];
        for case in cases {
            let cmdline = case
                .iter()
                .map(|a| quote_arg(a))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(split_cmdline(&cmdline), case, "cmdline: {cmdline}");
        }
    }
}
