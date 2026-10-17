/*
 * WiVRn VR streaming
 * Copyright (C) 2022  Guillaume Meunier <guillaume.meunier@centraliens.net>
 * Copyright (C) 2022  Patrick Nicolas <patricknicolas@laposte.net>
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

#include "swapchain.h"
#include "application.h"
#include "details/enumerate.h"
#include "session.h"

xr::swapchain::swapchain(
        xr::instance & inst,
        xr::session & s,
        vk::raii::Device & device,
        vk::Format format,
        int32_t width,
        int32_t height,
        int sample_count,
        uint32_t array_size,
        vk::Format view_format) :
        width_(width),
        height_(height),
        sample_count_(sample_count),
        format_(format),
        view_format_(format)
{
	assert(sample_count == 1);

	XrSwapchainUsageFlags usage_flags;

	switch (format)
	{
		case vk::Format::eD16Unorm:
		case vk::Format::eD16UnormS8Uint:
		case vk::Format::eD24UnormS8Uint:
		case vk::Format::eX8D24UnormPack32:
		case vk::Format::eD32Sfloat:
		case vk::Format::eD32SfloatS8Uint:
			usage_flags = XR_SWAPCHAIN_USAGE_SAMPLED_BIT | XR_SWAPCHAIN_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT;
			break;
		default:
			usage_flags = XR_SWAPCHAIN_USAGE_SAMPLED_BIT | XR_SWAPCHAIN_USAGE_COLOR_ATTACHMENT_BIT;
			break;
	}

	XrSwapchainCreateInfo create_info{
	        .type = XR_TYPE_SWAPCHAIN_CREATE_INFO,
	        .createFlags = 0,
	        .usageFlags = usage_flags,
	        .format = static_cast<VkFormat>(format),
	        .sampleCount = (uint32_t)sample_count,
	        .width = (uint32_t)width,
	        .height = (uint32_t)height,
	        .faceCount = 1,
	        .arraySize = array_size,
	        .mipCount = 1,
	};

	std::array<VkFormat, 2> view_formats{static_cast<VkFormat>(format), static_cast<VkFormat>(view_format)};
	XrVulkanSwapchainFormatListCreateInfoKHR format_list{
	        .type = XR_TYPE_VULKAN_SWAPCHAIN_FORMAT_LIST_CREATE_INFO_KHR,
	        .viewFormatCount = view_formats.size(),
	        .viewFormats = view_formats.data(),
	};
	if (view_format != vk::Format::eUndefined and inst.has_extension(XR_KHR_VULKAN_SWAPCHAIN_FORMAT_LIST_EXTENSION_NAME))
	{
		create_info.usageFlags |= XR_SWAPCHAIN_USAGE_MUTABLE_FORMAT_BIT;
		create_info.next = &format_list;
		view_format_ = view_format;
	}

	CHECK_XR(xrCreateSwapchain(s, &create_info, &id));

	auto images = details::enumerate<XrSwapchainImageVulkanKHR>(xrEnumerateSwapchainImages, id);

	images_.reserve(images.size());
	for (auto & image: images)
		images_.push_back(image.image);
}

int xr::swapchain::acquire()
{
	uint32_t index;

	XrSwapchainImageAcquireInfo acquire_info{
	        .type = XR_TYPE_SWAPCHAIN_IMAGE_ACQUIRE_INFO,
	};

	auto lock = application::get_queue().lock();
	CHECK_XR(xrAcquireSwapchainImage(id, &acquire_info, &index));

	return index;
}

bool xr::swapchain::wait(XrDuration timeout)
{
	XrSwapchainImageWaitInfo wait_info{
	        .type = XR_TYPE_SWAPCHAIN_IMAGE_WAIT_INFO,
	        .timeout = timeout,
	};

	XrResult result = xrWaitSwapchainImage(id, &wait_info);
	CHECK_XR(result, "xrWaitSwapchainImage");
	return result == XR_SUCCESS;
}

void xr::swapchain::release()
{
	XrSwapchainImageReleaseInfo release_info{
	        .type = XR_TYPE_SWAPCHAIN_IMAGE_RELEASE_INFO,
	};

	auto lock = application::get_queue().lock();
	CHECK_XR(xrReleaseSwapchainImage(id, &release_info));
}
