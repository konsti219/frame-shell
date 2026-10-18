/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#pragma once

// frame-monado's XR_FRAME_foveated_projection: a projection view's subImage holds a foveated image, per axis segment i
// covers pixelCounts[i] image pixels that each stand for |centerSegment - i| + 1 view pixels

#include <openxr/openxr.h>

#define XR_FRAME_FOVEATED_PROJECTION_EXTENSION_NAME "XR_FRAME_foveated_projection"
#define XR_TYPE_COMPOSITION_LAYER_PROJECTION_FOVEATION_FRAME ((XrStructureType)1000999001)
#define XR_FOVEATION_MAX_SEGMENTS_FRAME 24

struct XrFoveationAxisFRAME
{
	uint32_t centerSegment;
	uint32_t segmentCount;
	uint16_t pixelCounts[XR_FOVEATION_MAX_SEGMENTS_FRAME];
};

struct XrCompositionLayerProjectionFoveationFRAME
{
	XrStructureType type;
	const void * next;
	XrFoveationAxisFRAME x;
	XrFoveationAxisFRAME y;
};
