//
//  VDDevice.h
//  VDCamera
//
//  Created by John Boiles  on 4/10/20.
//
//  VDCamera is free software, and use is bound by the terms
//  set out in the LICENSE file distributed with this project.

#import <Foundation/Foundation.h>

#import "VDObjectStore.h"

NS_ASSUME_NONNULL_BEGIN

@interface VDDevice : NSObject <CMIOObject>

@property CMIOObjectID objectId;
@property CMIOObjectID pluginId;
@property CMIOObjectID streamId;

@end

NS_ASSUME_NONNULL_END
