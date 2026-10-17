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

#pragma once

#include "vk/allocation.h"
#include "wivrn_packets.h"
#include "xrt/xrt_defines.h"

#include <mutex>
#include <vulkan/vulkan_raii.hpp>

struct render_resources;

namespace wivrn
{

struct clock_offset;
struct vk_bundle;

class foveation
{
	std::mutex mutex;

	vk::Extent3D foveated_size; // per eye

	// Natural vertical gaze angle
	const float angle_offset;
	// Optionally defined from environment variables
	const float convergence_distance;

	// Eye poses relative to the head (view space); the views may be rotated (cant, tilt, panel roll)
	xrt_pose eye[2] = {XRT_POSE_IDENTITY, XRT_POSE_IDENTITY};
	xrt_quat gaze = {};
	from_headset::override_foveation_center manual_foveation = {};
	std::array<foveation_parameter, 2> params;
	std::array<foveation_description, 2> description;

	buffer_allocation gpu_buffer;
	vk::raii::Sampler sampler;

	vk::raii::DescriptorSetLayout ds_layout;
	vk::raii::PipelineLayout layout;
	std::array<vk::raii::Pipeline, 2> pipeline;
	vk::raii::DescriptorPool descriptor_pool;
	vk::DescriptorSet descriptor_set;

	// parameters used for last computation
	struct P
	{
		xrt_quat gaze = {};
		bool flip_y = false;
		xrt_rect src[2] = {};
		xrt_fov fovs[2] = {};
		xrt_pose eye[2] = {XRT_POSE_IDENTITY, XRT_POSE_IDENTITY};

		from_headset::override_foveation_center manual_foveation = {};
	};
	P last;

	// must hold lock on mutex to call it
	void compute_params();

	void update_ubo(
	        vk::raii::CommandBuffer & cmd,
	        bool flip_y,
	        std::array<xrt_rect, 2> src_rect,
	        std::array<xrt_fov, 2> src_fov,
	        std::array<xrt_pose, 2> src_pose);

public:
	foveation(wivrn::vk_bundle &,
	          vk::Extent3D foveated_size);

	void resize(wivrn::vk_bundle &, vk::Extent3D foveated_size);
	void update_tracking(const from_headset::tracking &);
	void update_foveation_center_override(const from_headset::override_foveation_center &);

	std::array<foveation_description, 2> foveate(
	        vk::raii::Device &,
	        vk::raii::CommandBuffer & cmd,
	        vk::ImageView y,
	        vk::ImageView cbcr,
	        bool flip_y,
	        std::array<vk::ImageView, 2> src,
	        std::array<xrt_rect, 2> src_rect,
	        std::array<xrt_fov, 2> src_fov,
	        std::array<xrt_pose, 2> src_pose,
	        bool alpha);
};
} // namespace wivrn
