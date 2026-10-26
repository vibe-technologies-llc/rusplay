# Scaling
- [S] `scale-dmabuf-gpu-fence-wait`: wait for a capture DMA-BUF's write fence on the GPU instead of polling it on the host thread (`wait_for_writes`): export it with `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`, import it as a temporary binary semaphore and gate the next submit with `hal::vulkan::Queue::add_wait_semaphore`; wgpu 30 does not enable `VK_KHR_external_semaphore_fd`, so the host's device would have to be opened through `wgpu::hal::vulkan::Adapter::open_with_callback` adding it, and the semaphore destroyed only after that submission completes

# Colour
- [M] `client-color-lut`: bake the client's linearise, tone map and gamut map into a 65³ `rgba16float` 3D LUT over a PQ-shaped input (rebuilt when the source or target changes, tetrahedral lookup) and fuse it into the unpack pass; today an HDR frame takes three source-resolution passes (unpack, `Lineariser`, analytic `ColorMapper`), and perceptual HDR mapping alone is about 2 ms at 4K on an RX 9070
