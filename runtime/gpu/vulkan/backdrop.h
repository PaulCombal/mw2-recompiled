#pragma once
// The system's screens (screen.h) as the window shows them: the game behind,
// blurred and darkened, and the screen's picture over the middle with rounded
// corners and a shadow, faded in and out. One draw, into the swapchain image
// after the frame was copied into it.
//
// The blur is the frame copied down a chain of halvings, each a linear blit,
// and read back at the smallest with a tent of nine taps. Two fixed shaders,
// written as SPIR-V here as the translator writes the title's.
#ifdef MW2_HAVE_VULKAN
#include <cstdint>
#include <functional>
#include <vulkan/vulkan.h>

namespace vk::backdrop
{
    // For the swapchain's format, on the presenter's device. False when it
    // cannot be made: the presenter then copies the picture over as it is.
    bool Create(VkDevice device, VkPhysicalDevice physical, VkFormat format);
    void Destroy();

    struct Draw
    {
        VkCommandBuffer commands;
        uint32_t slot;                    // the presenter's frame in flight
        VkImage target;                   // TRANSFER_DST, and left so
        VkExtent2D extent;
        // The game's frame, TRANSFER_SRC, and where it was copied in the
        // window. With no image the blur of the last one is shown where that
        // was, or black before there was any.
        VkImage frame;
        uint32_t frameWidth, frameHeight;
        VkOffset2D frameFrom, frameTo;
        // The picture, SHADER_READ_ONLY, and where it sits in the window
        // when fully shown; outline is its frame's width in pixels.
        VkImageView picture;
        VkOffset2D pictureFrom, pictureTo;
        float outline;
        float shown;                      // 0 to 1, the fade
        // Waits for the presenter's other frames, before an image they may
        // read is replaced.
        std::function<void()> settle;
    };
    void Record(const Draw& draw);
}
#endif
