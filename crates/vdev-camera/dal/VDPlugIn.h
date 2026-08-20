//
//  VDPlugIn.h
//  VDCamera
//
//  Created by John Boiles  on 4/9/20.
//
//  VDCamera is free software, and use is bound by the terms
//  set out in the LICENSE file distributed with this project.

#import <Foundation/Foundation.h>
#import <CoreMediaIO/CMIOHardwarePlugIn.h>

#import "VDObjectStore.h"

NS_ASSUME_NONNULL_BEGIN

@interface VDPlugIn : NSObject <CMIOObject>

@property CMIOObjectID objectId;

+ (VDPlugIn *)SharedPlugIn;

- (void)initialize;

- (void)teardown;

@end

NS_ASSUME_NONNULL_END
