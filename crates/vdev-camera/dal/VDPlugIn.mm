//
//  VDPlugIn.mm
//  VDCamera
//
//  Created by John Boiles  on 4/9/20.
//
//  VDCamera is free software, and use is bound by the terms
//  set out in the LICENSE file distributed with this project.

#import "VDPlugIn.h"

#import <CoreMediaIO/CMIOHardwarePlugIn.h>

#import "Logging.h"

@implementation VDPlugIn

+ (VDPlugIn *)SharedPlugIn {
    static VDPlugIn *sPlugIn = nil;
    static dispatch_once_t sOnceToken;
    dispatch_once(&sOnceToken, ^{
        sPlugIn = [[self alloc] init];
    });
    return sPlugIn;
}

- (void)initialize {
}

- (void)teardown {
}

#pragma mark - CMIOObject

- (BOOL)hasPropertyWithAddress:(CMIOObjectPropertyAddress)address {
    switch (address.mSelector) {
        case kCMIOObjectPropertyName:
            return true;
        default:
            DLog(@"VDPlugIn unhandled hasPropertyWithAddress for %@", [VDObjectStore StringFromPropertySelector:address.mSelector]);
            return false;
    };
}

- (BOOL)isPropertySettableWithAddress:(CMIOObjectPropertyAddress)address {
    switch (address.mSelector) {
        case kCMIOObjectPropertyName:
            return false;
        default:
            DLog(@"VDPlugIn unhandled isPropertySettableWithAddress for %@", [VDObjectStore StringFromPropertySelector:address.mSelector]);
            return false;
    };
}

- (UInt32)getPropertyDataSizeWithAddress:(CMIOObjectPropertyAddress)address qualifierDataSize:(UInt32)qualifierDataSize qualifierData:(const void*)qualifierData {
    switch (address.mSelector) {
        case kCMIOObjectPropertyName:
            return sizeof(CFStringRef);
        default:
            DLog(@"VDPlugIn unhandled getPropertyDataSizeWithAddress for %@", [VDObjectStore StringFromPropertySelector:address.mSelector]);
            return 0;
    };
}

- (void)getPropertyDataWithAddress:(CMIOObjectPropertyAddress)address qualifierDataSize:(UInt32)qualifierDataSize qualifierData:(nonnull const void *)qualifierData dataSize:(UInt32)dataSize dataUsed:(nonnull UInt32 *)dataUsed data:(nonnull void *)data {
    switch (address.mSelector) {
        case kCMIOObjectPropertyName:
            *static_cast<CFStringRef*>(data) = CFSTR("VDCamera Plugin");
            *dataUsed = sizeof(CFStringRef);
            return;
        default:
            DLog(@"VDPlugIn unhandled getPropertyDataWithAddress for %@", [VDObjectStore StringFromPropertySelector:address.mSelector]);
            return;
        };
}

- (void)setPropertyDataWithAddress:(CMIOObjectPropertyAddress)address qualifierDataSize:(UInt32)qualifierDataSize qualifierData:(nonnull const void *)qualifierData dataSize:(UInt32)dataSize data:(nonnull const void *)data {
    DLog(@"VDPlugIn unhandled setPropertyDataWithAddress for %@", [VDObjectStore StringFromPropertySelector:address.mSelector]);
}

@end
