/*
 * WiVRn VR streaming
 * Copyright (C) 2024  galister <galister@librevr.org>
 * Copyright (C) 2025  Patrick Nicolas <patricknicolas@laposte.net>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#include "foveation.h"
#include "wivrn_foveation.h"

#include "driver/xrt_cast.h"
#include "utils/wivrn_vk_bundle.h"
#include "vk/specialization_constants.h"
#include "wivrn_packets.h"

#include "math/m_api.h"
#include "xrt/xrt_defines.h"
#include "xrt/xrt_limits.h"
#include "util/u_logging.h"

#include <array>
#include <cmath>
#include <ranges>
#include <utility>
#include <vulkan/vulkan_raii.hpp>
#include <vulkan/vulkan_structs.hpp>
#include <openxr/openxr.h>

#define RENDER_FOVEATION_BUFFER_DIMENSIONS (4096 + 1)

namespace
{
struct ubo_data
{
	uint32_t x[XRT_MAX_VIEWS * RENDER_FOVEATION_BUFFER_DIMENSIONS];
	uint32_t y[XRT_MAX_VIEWS * RENDER_FOVEATION_BUFFER_DIMENSIONS];
};

vk::raii::Sampler make_sampler(wivrn::vk_bundle & vk)
{
	vk::raii::Sampler res(
	        vk.device,
	        vk::SamplerCreateInfo{
	                .magFilter = vk::Filter::eLinear,
	                .minFilter = vk::Filter::eLinear,
	                .mipmapMode = vk::SamplerMipmapMode::eLinear,
	                .addressModeU = vk::SamplerAddressMode::eClampToBorder,
	                .addressModeV = vk::SamplerAddressMode::eClampToBorder,
	                .addressModeW = vk::SamplerAddressMode::eClampToBorder,
	                .borderColor = vk::BorderColor::eFloatOpaqueBlack,
	        });
	vk.name(res, "foveation sampler");
	return res;
}

vk::raii::DescriptorSetLayout make_ds_layout(wivrn::vk_bundle & vk)
{
	std::array bindings{
	        vk::DescriptorSetLayoutBinding{
	                .binding = 0,
	                .descriptorType = vk::DescriptorType::eCombinedImageSampler,
	                .descriptorCount = 2,
	                .stageFlags = vk::ShaderStageFlagBits::eCompute,
	        },
	        vk::DescriptorSetLayoutBinding{
	                .binding = 1,
	                .descriptorType = vk::DescriptorType::eStorageBuffer,
	                .descriptorCount = 1,
	                .stageFlags = vk::ShaderStageFlagBits::eCompute,
	        },
	        vk::DescriptorSetLayoutBinding{
	                .binding = 2,
	                .descriptorType = vk::DescriptorType::eStorageImage,
	                .descriptorCount = 1,
	                .stageFlags = vk::ShaderStageFlagBits::eCompute,
	        },
	        vk::DescriptorSetLayoutBinding{
	                .binding = 3,
	                .descriptorType = vk::DescriptorType::eStorageImage,
	                .descriptorCount = 1,
	                .stageFlags = vk::ShaderStageFlagBits::eCompute,
	        },
	};
	vk::raii::DescriptorSetLayout res{
	        vk.device,
	        vk::DescriptorSetLayoutCreateInfo{
	                .bindingCount = bindings.size(),
	                .pBindings = bindings.data(),
	        },
	};
	vk.name(*res, "foveation descriptor set layout");
	return res;
}

vk::raii::PipelineLayout make_layout(wivrn::vk_bundle & vk, vk::DescriptorSetLayout ds_layout)
{
	vk::raii::PipelineLayout res(vk.device,
	                             vk::PipelineLayoutCreateInfo{
	                                     .setLayoutCount = 1,
	                                     .pSetLayouts = &ds_layout,
	                             });
	vk.name(*res, "foveation pipeline layout");
	return res;
}

std::array<vk::raii::Pipeline, 2> make_pipelines(wivrn::vk_bundle & vk, vk::PipelineLayout layout, int32_t alpha_width)
{
	auto shader = vk.load_shader("foveation");
	auto spc = make_specialization_constants(alpha_width);
	std::array res{
	        vk::raii::Pipeline{
	                vk.device,
	                nullptr,
	                vk::ComputePipelineCreateInfo{
	                        .stage = {
	                                .stage = vk::ShaderStageFlagBits::eCompute,
	                                .module = *shader,
	                                .pName = "main",
	                        },
	                        .layout = layout,
	                },
	        },
	        vk::raii::Pipeline{
	                vk.device,
	                nullptr,
	                vk::ComputePipelineCreateInfo{
	                        .stage = {
	                                .stage = vk::ShaderStageFlagBits::eCompute,
	                                .module = *shader,
	                                .pName = "main",
	                                .pSpecializationInfo = spc,
	                        },
	                        .layout = layout,
	                },
	        },
	};
	vk.name(*res[0], "foveation pipeline");
	vk.name(*res[1], "foveation+alpha pipeline");
	return res;
}

vk::raii::DescriptorPool make_ds_pool(wivrn::vk_bundle & vk)
{
	std::array pool_sizes{
	        vk::DescriptorPoolSize{
	                .type = vk::DescriptorType::eCombinedImageSampler,
	                .descriptorCount = 2,
	        },
	        vk::DescriptorPoolSize{
	                .type = vk::DescriptorType::eStorageImage,
	                .descriptorCount = 2,
	        },
	        vk::DescriptorPoolSize{
	                .type = vk::DescriptorType::eStorageBuffer,
	                .descriptorCount = 1,
	        },
	};
	vk::raii::DescriptorPool res{
	        vk.device,
	        vk::DescriptorPoolCreateInfo{
	                .maxSets = 5,
	                .poolSizeCount = pool_sizes.size(),
	                .pPoolSizes = pool_sizes.data(),
	        },
	};
	vk.name(*res, "foveation descriptor pool");
	return res;
}
uint32_t divide_and_round_up(uint32_t a, uint32_t b)
{
	return (a + (b - 1)) / b;
}

} // namespace

static bool is_zero_quat(xrt_quat q)
{
	return q.x == 0 and q.y == 0 and q.z == 0 and q.w == 0;
}

static xrt_vec2 yaw_pitch(xrt_quat q)
{
	if (is_zero_quat(q))
		return xrt_vec2{};

	float sine_theta = std::clamp(-2.0f * (q.y * q.z - q.w * q.x), -1.0f, 1.0f);

	float pitch = std::asin(sine_theta);

	if (std::abs(sine_theta) > 0.99999f)
	{
		float scale = std::copysign(2.0, sine_theta);
		return {scale * std::atan2(-q.z, q.w), pitch};
	}

	return {
	        std::atan2(2.0f * (q.x * q.z + q.w * q.y),
	                   q.w * q.w - q.x * q.x - q.y * q.y + q.z * q.z),
	        pitch};
}

static float angles_to_center(float e, float l, float r)
{
	e = tan(e);
	l = tan(l);
	r = tan(r);
	float res = std::clamp((e - l) / (r - l) * 2 - 1, -1.f, 1.f);
	// If the center isn't in the FoV, fallback to middle of image
	if (std::isnan(res))
		return 0;
	return res;
}

namespace wivrn
{

void foveation::compute_params()
{
	auto e = yaw_pitch(gaze); // yaw positive to the left, pitch positive up

	if (manual_foveation.enabled)
		e.y = manual_foveation.pitch;
	else if (is_zero_quat(gaze))
		e.y = -angle_offset; // Natural gaze is not straight forward

	// The gazed point in head space, then seen from each eye in its own view frame: the views can be rotated
	// relative to the head (cant, tilt, or the Steam Frame's rolled panels), and the fov is in that frame
	const float distance = manual_foveation.enabled ? manual_foveation.distance : convergence_distance;
	const xrt_vec3 target{
	        -std::sin(e.x) * std::cos(e.y) * distance,
	        std::sin(e.y) * distance,
	        -std::cos(e.x) * std::cos(e.y) * distance,
	};

	for (size_t i = 0; i < 2; ++i)
	{
		const auto & fov = last.fovs[i];

		const auto & view = last.eye[i];
		xrt_vec3 from_eye{target.x - view.position.x, target.y - view.position.y, target.z - view.position.z};
		xrt_quat to_view;
		math_quat_invert(&view.orientation, &to_view);
		xrt_vec3 in_view;
		math_quat_rotate_vec3(&to_view, &from_eye, &in_view);
		// Behind the eye (possible for odd poses): keep the middle of the image
		const bool ahead = in_view.z < 0;

		size_t extent_w = std::abs(last.src[i].extent.w);
		if (foveated_size.width < extent_w and ahead)
		{
			auto center = angles_to_center(std::atan2(in_view.x, -in_view.z), fov.angle_left, fov.angle_right);
			description[i].x = {uint16_t(foveated_size.width), uint16_t(extent_w), center};
		}
		else
			description[i].x = {uint16_t(foveated_size.width), uint16_t(extent_w), 0};

		size_t extent_h = std::abs(last.src[i].extent.h);
		if (foveated_size.height < extent_h and ahead)
		{
			auto center = angles_to_center(std::atan2(in_view.y, -in_view.z), fov.angle_up, fov.angle_down);
			description[i].y = {uint16_t(foveated_size.height), uint16_t(extent_h), center};
		}
		else
			description[i].y = {uint16_t(foveated_size.height), uint16_t(extent_h), 0};
		generate_foveation(description[i], params[i]);
	}
}

foveation::foveation(wivrn::vk_bundle & bundle, vk::Extent3D foveated_size) :
        foveated_size(foveated_size),
        // normal sight line is between 10° and 15° below horizontal
        // https://apps.dtic.mil/sti/tr/pdf/AD0758339.pdf pages 393-394
        // testing shows 10° looks better
        angle_offset(10 * M_PI / 180),
        convergence_distance(1 /* meter*/),
        gpu_buffer(
                bundle.device,
                {
                        .size = sizeof(ubo_data),
                        .usage = vk::BufferUsageFlagBits::eTransferDst | vk::BufferUsageFlagBits::eStorageBuffer,
                },
                VmaAllocationCreateInfo{
                        .flags = VMA_ALLOCATION_CREATE_HOST_ACCESS_SEQUENTIAL_WRITE_BIT,
                        .usage = VMA_MEMORY_USAGE_AUTO_PREFER_DEVICE,
                },
                "foveation storage buffer"),
        sampler(make_sampler(bundle)),
        ds_layout(make_ds_layout(bundle)),
        layout(make_layout(bundle, ds_layout)),
        pipeline(make_pipelines(bundle, layout, foveated_size.width / 2)),
        descriptor_pool(make_ds_pool(bundle)),
        descriptor_set(bundle.device.allocateDescriptorSets(vk::DescriptorSetAllocateInfo{
                .descriptorPool = descriptor_pool,
                .descriptorSetCount = 1,
                .pSetLayouts = &*ds_layout,
        })[0]
                               .release())
{
	bundle.name(descriptor_set, "foveation descriptor set");
}

void foveation::resize(wivrn::vk_bundle & bundle, vk::Extent3D size)
{
	std::lock_guard lock(mutex);
	foveated_size = size;
	pipeline = make_pipelines(bundle, layout, size.width / 2);
	// Force the parameters to be recomputed for the new size
	last = {};
}

void foveation::update_tracking(const from_headset::tracking & tracking)
{
	std::lock_guard lock(mutex);

	const uint8_t orientation_ok = from_headset::pose_flags::orientation_valid | from_headset::pose_flags::orientation_tracked;

	if (tracking.view_flags & XR_VIEW_STATE_POSITION_VALID_BIT)
	{
		eye[0] = xrt_cast(tracking.views[0].pose);
		eye[1] = xrt_cast(tracking.views[1].pose);
	}

	for (const auto & pose: tracking.device_poses)
	{
		if (pose.device != device_id::EYE_GAZE)
			continue;

		if ((pose.flags & orientation_ok) != orientation_ok)
			return;

		gaze = xrt_cast(pose.pose.orientation);
		// Once per server run: shows that eye-tracked (dynamic) foveation is in use
		static bool logged = false;
		if (not std::exchange(logged, true))
			U_LOG_I("Foveation follows the headset's eye gaze");
		return;
	}
}

void foveation::update_foveation_center_override(const from_headset::override_foveation_center & center)
{
	std::lock_guard lock(mutex);
	manual_foveation = center;
}

static void fill_ubo(
        std::span<uint32_t> ubo,
        const std::vector<uint16_t> & params,
        bool flip,
        size_t offset,
        size_t size,
        [[maybe_unused]] int count)
{
	assert(params.size() % 2 == 1);
	const int n_ratio = (params.size() - 1) / 2;
	ubo[0] = offset;
	if (flip)
		ubo[0] += size;
	for (auto [i, n]: std::ranges::enumerate_view(params))
	{
		const int n_source = std::abs(n_ratio - int(i)) + 1;
		for (size_t j = 0; j < n; ++j)
		{
			assert(count > 0);
			if (flip)
				ubo[1] = ubo[0] - n_source;
			else
				ubo[1] = ubo[0] + n_source;
			ubo = ubo.subspan(1);
			--count;
		}
	}
	if (not ubo.empty())
		std::ranges::fill(ubo, ubo[0]);
}

template <typename T>
static bool operator==(const T & a, const T & b)
{
	static_assert(std::has_unique_object_representations_v<T>);
	return std::memcmp(&a, &b, sizeof(T)) == 0;
}
static bool operator==(const xrt_quat & a, const xrt_quat & b)
{
	return a.x == b.x and a.y == b.y and a.z == b.z and a.w == b.w;
}
static bool operator==(const xrt_fov & a, const xrt_fov & b)
{
	return a.angle_left == b.angle_left and a.angle_right == b.angle_right and a.angle_up == b.angle_up and a.angle_down == b.angle_down;
}
static bool same_pose(const xrt_pose & a, const xrt_pose & b)
{
	return std::abs(a.position.x - b.position.x) < 0.0005 and std::abs(a.position.y - b.position.y) < 0.0005 and
	       std::abs(a.position.z - b.position.z) < 0.0005 and
	       std::abs(a.orientation.x * b.orientation.x + a.orientation.y * b.orientation.y +
	                a.orientation.z * b.orientation.z + a.orientation.w * b.orientation.w) > 0.99999f;
}

// The views the frame was rendered with, relative to the head. Apps may render the headset's views (rolled like the
// Frame's panels) or their own (VRChat renders upright views in worlds, the headset's on its loading screen).
static std::array<xrt_pose, 2> rendered_views(const xrt_pose (&eye)[2], const std::array<xrt_pose, 2> & src_pose)
{
	// The headset's views imply the same head orientation for both submitted views
	xrt_quat to_eye[2], head[2];
	for (size_t i = 0; i < 2; ++i)
	{
		math_quat_invert(&eye[i].orientation, &to_eye[i]);
		math_quat_rotate(&src_pose[i].orientation, &to_eye[i], &head[i]);
	}
	const float dot = head[0].x * head[1].x + head[0].y * head[1].y + head[0].z * head[1].z + head[0].w * head[1].w;
	if (std::abs(dot) > 0.9999f) // within ~1.6°
		return {eye[0], eye[1]};

	// Otherwise take the head as the views' mean orientation: the app's own views are nearly level
	xrt_quat mean, to_mean;
	math_quat_slerp(&src_pose[0].orientation, &src_pose[1].orientation, 0.5f, &mean);
	math_quat_invert(&mean, &to_mean);
	std::array<xrt_pose, 2> views{eye[0], eye[1]};
	for (size_t i = 0; i < 2; ++i)
		math_quat_rotate(&to_mean, &src_pose[i].orientation, &views[i].orientation);
	return views;
}

void foveation::update_ubo(
        vk::raii::CommandBuffer & cmd,
        bool flip_y,
        std::array<xrt_rect, 2> src_rect,
        std::array<xrt_fov, 2> src_fov,
        std::array<xrt_pose, 2> src_pose)
{
	// Check if the last value is still valid
	std::lock_guard lock(mutex);
	const auto views = rendered_views(eye, src_pose);
	if (last.flip_y == flip_y and
	    last.src[0] == src_rect[0] and
	    last.src[1] == src_rect[1] and
	    last.fovs[0] == src_fov[0] and
	    last.fovs[1] == src_fov[1] and
	    (last.gaze == gaze or manual_foveation.enabled) and // Ignore the gaze if foveation center is overridden
	    same_pose(last.eye[0], views[0]) and
	    same_pose(last.eye[1], views[1]) and
	    last.manual_foveation.enabled == manual_foveation.enabled and
	    std::abs(last.manual_foveation.pitch - manual_foveation.pitch) < 0.0005 and
	    std::abs(last.manual_foveation.distance - manual_foveation.distance) < 0.0005)
		return;

	last = {
	        .gaze = gaze,
	        .flip_y = flip_y,
	        .src = {src_rect[0], src_rect[1]},
	        .fovs = {src_fov[0], src_fov[1]},
	        .eye = {views[0], views[1]},
	        .manual_foveation = manual_foveation,
	};

	compute_params();

	ubo_data ubo;
	for (size_t view = 0; view < 2; ++view)
	{
		bool flip = false;
		size_t offset, extent;

		if (src_rect[view].extent.w < 0)
		{
			flip = true;
			offset = src_rect[view].offset.w + src_rect[view].extent.w;
			extent = -src_rect[view].extent.w;
		}
		else
		{
			offset = src_rect[view].offset.w;
			extent = src_rect[view].extent.w;
		}
		fill_ubo(std::span(ubo.x + view * RENDER_FOVEATION_BUFFER_DIMENSIONS, RENDER_FOVEATION_BUFFER_DIMENSIONS),
		         params[view].x,
		         flip,
		         offset,
		         extent,
		         foveated_size.width);

		if (src_rect[view].extent.h < 0)
		{
			flip = not flip_y;
			offset = src_rect[view].offset.h + src_rect[view].extent.h;
			extent = -src_rect[view].extent.h;
		}
		else
		{
			flip = flip_y;
			offset = src_rect[view].offset.h;
			extent = src_rect[view].extent.h;
		}
		fill_ubo(std::span(ubo.y + view * RENDER_FOVEATION_BUFFER_DIMENSIONS, RENDER_FOVEATION_BUFFER_DIMENSIONS),
		         params[view].y,
		         flip,
		         offset,
		         extent,
		         foveated_size.height);
	}
	vmaCopyMemoryToAllocation(vk_allocator::instance(), &ubo, gpu_buffer, 0, sizeof(ubo));
	std::memcpy(gpu_buffer.data<ubo_data>(), &ubo, sizeof(ubo));
}

std::array<foveation_description, 2> foveation::foveate(
        vk::raii::Device & device,
        vk::raii::CommandBuffer & cmd,
        vk::ImageView y,
        vk::ImageView cbcr,
        bool flip_y,
        std::array<vk::ImageView, 2> src,
        std::array<xrt_rect, 2> src_rect,
        std::array<xrt_fov, 2> src_fov,
        std::array<xrt_pose, 2> src_pose,
        bool alpha)
{
	update_ubo(cmd, flip_y, src_rect, src_fov, src_pose);

	std::array src_image_info{
	        vk::DescriptorImageInfo{
	                .sampler = *sampler,
	                .imageView = src[0],
	                .imageLayout = vk::ImageLayout::eShaderReadOnlyOptimal,
	        },
	        vk::DescriptorImageInfo{
	                .sampler = *sampler,
	                .imageView = src[1],
	                .imageLayout = vk::ImageLayout::eShaderReadOnlyOptimal,
	        },
	};

	vk::DescriptorBufferInfo ubo_info{
	        .buffer = gpu_buffer,
	        .range = vk::WholeSize,
	};

	vk::DescriptorImageInfo y_info{
	        .imageView = y,
	        .imageLayout = vk::ImageLayout::eGeneral,
	};
	vk::DescriptorImageInfo cbcr_info{
	        .imageView = cbcr,
	        .imageLayout = vk::ImageLayout::eGeneral,
	};

	std::array writes = {
	        vk::WriteDescriptorSet{
	                .dstSet = descriptor_set,
	                .dstBinding = 0,
	                .descriptorCount = src_image_info.size(),
	                .descriptorType = vk::DescriptorType::eCombinedImageSampler,
	                .pImageInfo = src_image_info.data(),
	        },
	        vk::WriteDescriptorSet{
	                .dstSet = descriptor_set,
	                .dstBinding = 1,
	                .descriptorCount = 1,
	                .descriptorType = vk::DescriptorType::eStorageBuffer,
	                .pBufferInfo = &ubo_info,
	        },
	        vk::WriteDescriptorSet{
	                .dstSet = descriptor_set,
	                .dstBinding = 2,
	                .descriptorCount = 1,
	                .descriptorType = vk::DescriptorType::eStorageImage,
	                .pImageInfo = &y_info,
	        },
	        vk::WriteDescriptorSet{
	                .dstSet = descriptor_set,
	                .dstBinding = 3,
	                .descriptorCount = 1,
	                .descriptorType = vk::DescriptorType::eStorageImage,
	                .pImageInfo = &cbcr_info,
	        },
	};

	device.updateDescriptorSets(writes, {});

	cmd.bindPipeline(vk::PipelineBindPoint::eCompute, *pipeline[alpha]);
	cmd.bindDescriptorSets(vk::PipelineBindPoint::eCompute, *layout, 0, descriptor_set, {});
	cmd.dispatch(divide_and_round_up(foveated_size.width, 8),
	             divide_and_round_up(foveated_size.height, 8),
	             2);

	return description;
}
} // namespace wivrn
