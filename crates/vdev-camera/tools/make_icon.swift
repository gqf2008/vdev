import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

let size = 1024
let cs = CGColorSpaceCreateDeviceRGB()
guard let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
                          bytesPerRow: 0, space: cs,
                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
    fatalError("no ctx")
}
ctx.setShouldAntialias(true)
ctx.clear(CGRect(x: 0, y: 0, width: size, height: size))

let S = CGFloat(size)
let margin: CGFloat = 44
let body = CGRect(x: margin, y: margin, width: S - 2*margin, height: S - 2*margin)
let radius = body.width * 0.2237

// squircle 底 + 渐变
let bodyPath = CGPath(roundedRect: body, cornerWidth: radius, cornerHeight: radius, transform: nil)
ctx.addPath(bodyPath)
ctx.clip()
let gradColors = [
    CGColor(red: 0.32, green: 0.20, blue: 0.62, alpha: 1),
    CGColor(red: 0.13, green: 0.15, blue: 0.30, alpha: 1),
    CGColor(red: 0.05, green: 0.07, blue: 0.16, alpha: 1),
] as CFArray
let grad = CGGradient(colorsSpace: cs, colors: gradColors, locations: [0.0, 0.55, 1.0])!
ctx.drawLinearGradient(grad,
                       start: CGPoint(x: body.midX, y: body.maxY),
                       end: CGPoint(x: body.midX, y: body.minY),
                       options: [])

// 顶部高光
let glossColors = [
    CGColor(red: 1, green: 1, blue: 1, alpha: 0.14),
    CGColor(red: 1, green: 1, blue: 1, alpha: 0.0),
] as CFArray
let gloss = CGGradient(colorsSpace: cs, colors: glossColors, locations: [0.0, 1.0])!
ctx.drawLinearGradient(gloss,
                       start: CGPoint(x: body.midX, y: body.maxY - body.height*0.02),
                       end: CGPoint(x: body.midX, y: body.midY),
                       options: [])

// 摄像头主体（白）
let camW = body.width * 0.60
let camH = body.height * 0.44
let camRect = CGRect(x: body.midX - camW/2, y: body.midY - camH/2, width: camW, height: camH)
ctx.setFillColor(CGColor(red: 0.96, green: 0.96, blue: 0.97, alpha: 0.95))
ctx.addPath(CGPath(roundedRect: camRect, cornerWidth: camH*0.18, cornerHeight: camH*0.18, transform: nil))
ctx.fillPath()

// 取景器凸起
let bumpW = camW * 0.17
let bumpH = camH * 0.11
let bumpRect = CGRect(x: camRect.maxX - bumpW*2.1, y: camRect.maxY + bumpH*0.7, width: bumpW, height: bumpH)
ctx.addPath(CGPath(roundedRect: bumpRect, cornerWidth: bumpH*0.5, cornerHeight: bumpH*0.5, transform: nil))
ctx.fillPath()

// 镜头外环
let ringD = camH * 0.56
let ringRect = CGRect(x: camRect.midX - ringD/2, y: camRect.midY - ringD/2, width: ringD, height: ringD)
let ringColors = [
    CGColor(red: 0.35, green: 0.35, blue: 0.40, alpha: 1),
    CGColor(red: 0.10, green: 0.10, blue: 0.14, alpha: 1),
] as CFArray
let ringGrad = CGGradient(colorsSpace: cs, colors: ringColors, locations: [0.0, 1.0])!
ctx.saveGState()
ctx.addEllipse(in: ringRect)
ctx.clip()
ctx.drawLinearGradient(ringGrad,
                       start: CGPoint(x: ringRect.minX, y: ringRect.minY),
                       end: CGPoint(x: ringRect.maxX, y: ringRect.maxY),
                       options: [])
ctx.restoreGState()
ctx.setStrokeColor(CGColor(red: 0.80, green: 0.80, blue: 0.85, alpha: 0.9))
ctx.setLineWidth(ringD * 0.025)
ctx.strokeEllipse(in: ringRect.insetBy(dx: ringD*0.035, dy: ringD*0.035))

// 镜片
let lensD = ringD * 0.72
let lensRect = CGRect(x: ringRect.midX - lensD/2, y: ringRect.midY - lensD/2, width: lensD, height: lensD)
let lensColors = [
    CGColor(red: 0.36, green: 0.24, blue: 0.64, alpha: 1),
    CGColor(red: 0.12, green: 0.15, blue: 0.32, alpha: 1),
] as CFArray
let lensGrad = CGGradient(colorsSpace: cs, colors: lensColors, locations: [0.0, 1.0])!
ctx.saveGState()
ctx.addEllipse(in: lensRect)
ctx.clip()
ctx.drawLinearGradient(lensGrad,
                       start: CGPoint(x: lensRect.minX, y: lensRect.minY),
                       end: CGPoint(x: lensRect.maxX, y: lensRect.maxY),
                       options: [])
ctx.restoreGState()

// 高光
let hlD = lensD * 0.34
ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.30))
ctx.fillEllipse(in: CGRect(x: lensRect.minX + lensD*0.16, y: lensRect.minY + lensD*0.60, width: hlD, height: hlD))

// 代码双箭头
func chevron(cx: CGFloat, cy: CGFloat, s: CGFloat, lw: CGFloat) {
    let p = CGMutablePath()
    p.move(to: CGPoint(x: cx - s/2, y: cy + s/2))
    p.addLine(to: CGPoint(x: cx + s/2, y: cy))
    p.addLine(to: CGPoint(x: cx - s/2, y: cy - s/2))
    ctx.addPath(p)
    ctx.setStrokeColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.92))
    ctx.setLineWidth(lw)
    ctx.setLineCap(.round)
    ctx.setLineJoin(.round)
    ctx.strokePath()
}
let cs_ = lensD * 0.30
let cw = lensD * 0.085
chevron(cx: lensRect.midX - lensD*0.13, cy: lensRect.midY, s: cs_, lw: cw)
chevron(cx: lensRect.midX + lensD*0.13, cy: lensRect.midY, s: cs_, lw: cw)

// 状态灯（红黄绿）
let dotD = camH * 0.035
let dotColors: [CGColor] = [
    CGColor(red: 0.95, green: 0.30, blue: 0.30, alpha: 0.95),
    CGColor(red: 0.95, green: 0.75, blue: 0.20, alpha: 0.95),
    CGColor(red: 0.30, green: 0.80, blue: 0.40, alpha: 0.95),
]
for (i, c) in dotColors.enumerated() {
    let dotX = camRect.minX + camH * 0.22 + CGFloat(i) * camH * 0.10
    let dotY = camRect.minY + camH * 0.16
    ctx.setFillColor(c)
    ctx.fillEllipse(in: CGRect(x: dotX, y: dotY, width: dotD, height: dotD))
}

// 输出 PNG
guard let img = ctx.makeImage() else { fatalError("no image") }
let url = URL(fileURLWithPath: "/tmp/vdev-icon.png") as CFURL
guard let dest = CGImageDestinationCreateWithURL(url, UTType.png.identifier as CFString, 1, nil) else {
    fatalError("no dest")
}
CGImageDestinationAddImage(dest, img, nil)
guard CGImageDestinationFinalize(dest) else { fatalError("finalize failed") }
print("wrote /tmp/vdev-icon.png")
