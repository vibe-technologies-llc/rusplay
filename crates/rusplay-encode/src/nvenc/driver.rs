use std::{
    ffi::{c_int, c_void},
    os::fd::{FromRawFd, IntoRawFd, OwnedFd},
    ptr,
    rc::Rc,
};

use libloading::Library;
use rusplay_core::Resolution;

use crate::{
    ApiVersion, Backend, CudaCall, CudaStatus, EncodeError, NvencCall, NvencStatus,
    nvenc::ffi::{self, Plain},
};

const BACKEND: Backend = Backend::Nvenc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LibraryNames {
    pub cuda: &'static str,
    pub nvenc: &'static str,
}

pub(super) const SYSTEM_LIBRARIES: LibraryNames = LibraryNames {
    cuda: "libcuda.so.1",
    nvenc: "libnvidia-encode.so.1",
};

pub(super) const REQUIRED_API: ApiVersion = ApiVersion {
    major: ffi::API_MAJOR,
    minor: ffi::API_MINOR,
};

pub(super) const fn api_version_of(packed: u32) -> ApiVersion {
    ApiVersion {
        major: packed >> 4,
        minor: packed & 0xf,
    }
}

fn cuda(call: CudaCall, result: ffi::CuResult) -> Result<(), EncodeError> {
    if result == ffi::CUDA_SUCCESS {
        Ok(())
    } else {
        Err(EncodeError::Cuda {
            call,
            status: CudaStatus(result),
        })
    }
}

fn nvenc(call: NvencCall, status: ffi::Status) -> Result<(), EncodeError> {
    if status == ffi::SUCCESS {
        Ok(())
    } else {
        Err(EncodeError::Nvenc {
            call,
            status: NvencStatus(status),
        })
    }
}

fn open_library(library: &'static str) -> Result<Library, EncodeError> {
    unsafe { Library::new(library) }
        .map_err(|source| EncodeError::LibraryNotFound { library, source })
}

fn symbol<T: Copy>(
    library: &Library,
    name: &'static str,
    symbol: &'static str,
) -> Result<T, EncodeError> {
    unsafe { library.get::<T>(symbol) }
        .map(|found| *found)
        .map_err(|source| EncodeError::SymbolMissing {
            library: name,
            symbol,
            source,
        })
}

fn required<T>(function: Option<T>, name: &'static str) -> Result<T, EncodeError> {
    function.ok_or(EncodeError::DriverFunctionMissing {
        backend: BACKEND,
        function: name,
    })
}

struct CudaApi {
    init: ffi::CuInitFn,
    device_get_count: ffi::CuDeviceGetCountFn,
    device_get: ffi::CuDeviceGetFn,
    ctx_create: ffi::CuCtxCreateFn,
    ctx_destroy: ffi::CuCtxDestroyFn,
    ctx_push_current: ffi::CuCtxPushCurrentFn,
    ctx_pop_current: ffi::CuCtxPopCurrentFn,
    import_external_memory: ffi::CuImportExternalMemoryFn,
    external_memory_get_mapped_buffer: ffi::CuExternalMemoryGetMappedBufferFn,
    destroy_external_memory: ffi::CuDestroyExternalMemoryFn,
    mem_free: ffi::CuMemFreeFn,
}

impl CudaApi {
    fn load(library: &Library, name: &'static str) -> Result<Self, EncodeError> {
        Ok(Self {
            init: symbol(library, name, "cuInit")?,
            device_get_count: symbol(library, name, "cuDeviceGetCount")?,
            device_get: symbol(library, name, "cuDeviceGet")?,
            ctx_create: symbol(library, name, "cuCtxCreate_v2")?,
            ctx_destroy: symbol(library, name, "cuCtxDestroy_v2")?,
            ctx_push_current: symbol(library, name, "cuCtxPushCurrent_v2")?,
            ctx_pop_current: symbol(library, name, "cuCtxPopCurrent_v2")?,
            import_external_memory: symbol(library, name, "cuImportExternalMemory")?,
            external_memory_get_mapped_buffer: symbol(
                library,
                name,
                "cuExternalMemoryGetMappedBuffer",
            )?,
            destroy_external_memory: symbol(library, name, "cuDestroyExternalMemory")?,
            mem_free: symbol(library, name, "cuMemFree_v2")?,
        })
    }
}

struct NvencApi {
    open_encode_session_ex: ffi::OpenEncodeSessionExFn,
    get_encode_guid_count: ffi::GetEncodeGuidCountFn,
    get_encode_guids: ffi::GetEncodeGuidsFn,
    get_input_format_count: ffi::GetInputFormatCountFn,
    get_input_formats: ffi::GetInputFormatsFn,
    get_encode_caps: ffi::GetEncodeCapsFn,
    get_encode_preset_config_ex: ffi::GetEncodePresetConfigExFn,
    initialize_encoder: ffi::InitializeEncoderFn,
    reconfigure_encoder: ffi::ReconfigureEncoderFn,
    create_input_buffer: ffi::CreateInputBufferFn,
    destroy_input_buffer: ffi::DestroyBufferFn,
    lock_input_buffer: ffi::LockInputBufferFn,
    unlock_input_buffer: ffi::DestroyBufferFn,
    create_bitstream_buffer: ffi::CreateBitstreamBufferFn,
    destroy_bitstream_buffer: ffi::DestroyBufferFn,
    lock_bitstream: ffi::LockBitstreamFn,
    unlock_bitstream: ffi::DestroyBufferFn,
    register_resource: ffi::RegisterResourceFn,
    unregister_resource: ffi::DestroyBufferFn,
    map_input_resource: ffi::MapInputResourceFn,
    unmap_input_resource: ffi::DestroyBufferFn,
    encode_picture: ffi::EncodePictureFn,
    destroy_encoder: ffi::DestroyEncoderFn,
}

impl NvencApi {
    fn load(library: &Library, name: &'static str) -> Result<Self, EncodeError> {
        let max_supported: ffi::GetMaxSupportedVersionFn =
            symbol(library, name, "NvEncodeAPIGetMaxSupportedVersion")?;
        let create_instance: ffi::CreateInstanceFn =
            symbol(library, name, "NvEncodeAPICreateInstance")?;

        let mut supported = 0;
        nvenc(NvencCall::GetMaxSupportedVersion, unsafe {
            max_supported(&raw mut supported)
        })?;
        if supported < ffi::MAX_SUPPORTED_VERSION {
            return Err(EncodeError::DriverTooOld {
                backend: BACKEND,
                required: REQUIRED_API,
                supported: api_version_of(supported),
            });
        }

        let mut list = ffi::FunctionList::zeroed();
        list.version = ffi::FUNCTION_LIST_VER;
        nvenc(NvencCall::CreateInstance, unsafe {
            create_instance(&raw mut list)
        })?;

        Ok(Self {
            open_encode_session_ex: required(
                list.open_encode_session_ex,
                "nvEncOpenEncodeSessionEx",
            )?,
            get_encode_guid_count: required(list.get_encode_guid_count, "nvEncGetEncodeGUIDCount")?,
            get_encode_guids: required(list.get_encode_guids, "nvEncGetEncodeGUIDs")?,
            get_input_format_count: required(
                list.get_input_format_count,
                "nvEncGetInputFormatCount",
            )?,
            get_input_formats: required(list.get_input_formats, "nvEncGetInputFormats")?,
            get_encode_caps: required(list.get_encode_caps, "nvEncGetEncodeCaps")?,
            get_encode_preset_config_ex: required(
                list.get_encode_preset_config_ex,
                "nvEncGetEncodePresetConfigEx",
            )?,
            initialize_encoder: required(list.initialize_encoder, "nvEncInitializeEncoder")?,
            reconfigure_encoder: required(list.reconfigure_encoder, "nvEncReconfigureEncoder")?,
            create_input_buffer: required(list.create_input_buffer, "nvEncCreateInputBuffer")?,
            destroy_input_buffer: required(list.destroy_input_buffer, "nvEncDestroyInputBuffer")?,
            lock_input_buffer: required(list.lock_input_buffer, "nvEncLockInputBuffer")?,
            unlock_input_buffer: required(list.unlock_input_buffer, "nvEncUnlockInputBuffer")?,
            create_bitstream_buffer: required(
                list.create_bitstream_buffer,
                "nvEncCreateBitstreamBuffer",
            )?,
            destroy_bitstream_buffer: required(
                list.destroy_bitstream_buffer,
                "nvEncDestroyBitstreamBuffer",
            )?,
            lock_bitstream: required(list.lock_bitstream, "nvEncLockBitstream")?,
            unlock_bitstream: required(list.unlock_bitstream, "nvEncUnlockBitstream")?,
            register_resource: required(list.register_resource, "nvEncRegisterResource")?,
            unregister_resource: required(list.unregister_resource, "nvEncUnregisterResource")?,
            map_input_resource: required(list.map_input_resource, "nvEncMapInputResource")?,
            unmap_input_resource: required(list.unmap_input_resource, "nvEncUnmapInputResource")?,
            encode_picture: required(list.encode_picture, "nvEncEncodePicture")?,
            destroy_encoder: required(list.destroy_encoder, "nvEncDestroyEncoder")?,
        })
    }
}

pub(super) struct Driver {
    cuda: CudaApi,
    nvenc: NvencApi,
    _nvenc_library: Library,
    _cuda_library: Library,
}

impl Driver {
    pub(super) fn load(names: LibraryNames) -> Result<Rc<Self>, EncodeError> {
        let cuda_library = open_library(names.cuda)?;
        let nvenc_library = open_library(names.nvenc)?;

        let cuda = CudaApi::load(&cuda_library, names.cuda)?;
        let nvenc = NvencApi::load(&nvenc_library, names.nvenc)?;

        Ok(Rc::new(Self {
            cuda,
            nvenc,
            _nvenc_library: nvenc_library,
            _cuda_library: cuda_library,
        }))
    }
}

pub(super) struct CudaContext {
    driver: Rc<Driver>,
    context: ffi::CuContext,
}

impl CudaContext {
    pub(super) fn new(driver: &Rc<Driver>, gpu_index: u32) -> Result<Rc<Self>, EncodeError> {
        let api = &driver.cuda;
        cuda(CudaCall::Init, unsafe { (api.init)(0) })?;

        let mut count: c_int = 0;
        cuda(CudaCall::DeviceGetCount, unsafe {
            (api.device_get_count)(&raw mut count)
        })?;
        let count = u32::try_from(count).unwrap_or(0);
        let ordinal = c_int::try_from(gpu_index)
            .ok()
            .filter(|_| gpu_index < count)
            .ok_or(EncodeError::GpuIndexOutOfRange {
                index: gpu_index,
                count,
            })?;

        let mut device: ffi::CuDevice = 0;
        cuda(CudaCall::DeviceGet, unsafe {
            (api.device_get)(&raw mut device, ordinal)
        })?;

        let mut context: ffi::CuContext = ptr::null_mut();
        cuda(CudaCall::CtxCreate, unsafe {
            (api.ctx_create)(&raw mut context, 0, device)
        })?;
        let created = Rc::new(Self {
            driver: Rc::clone(driver),
            context,
        });

        let mut popped: ffi::CuContext = ptr::null_mut();
        cuda(CudaCall::CtxPopCurrent, unsafe {
            (api.ctx_pop_current)(&raw mut popped)
        })?;

        Ok(created)
    }

    pub(super) fn enter(&self) -> Result<Current<'_>, EncodeError> {
        cuda(CudaCall::CtxPushCurrent, unsafe {
            (self.driver.cuda.ctx_push_current)(self.context)
        })?;

        Ok(Current { context: self })
    }
}

impl Drop for CudaContext {
    fn drop(&mut self) {
        unsafe { (self.driver.cuda.ctx_destroy)(self.context) };
    }
}

pub(super) struct Current<'a> {
    context: &'a CudaContext,
}

impl Drop for Current<'_> {
    fn drop(&mut self) {
        let mut popped: ffi::CuContext = ptr::null_mut();
        unsafe { (self.context.driver.cuda.ctx_pop_current)(&raw mut popped) };
    }
}

pub(super) struct Encoder {
    context: Rc<CudaContext>,
    handle: ffi::EncoderPtr,
}

impl Encoder {
    pub(super) fn open(context: Rc<CudaContext>) -> Result<Rc<Self>, EncodeError> {
        let mut params = ffi::OpenEncodeSessionExParams::zeroed();
        params.version = ffi::OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        params.device_type = ffi::DEVICE_TYPE_CUDA;
        params.device = context.context;
        params.api_version = ffi::API_VERSION;

        let mut handle: ffi::EncoderPtr = ptr::null_mut();
        let api = &context.driver.nvenc;
        nvenc(NvencCall::OpenEncodeSessionEx, unsafe {
            (api.open_encode_session_ex)(&raw mut params, &raw mut handle)
        })?;

        Ok(Rc::new(Self { context, handle }))
    }

    fn api(&self) -> &NvencApi {
        &self.context.driver.nvenc
    }

    pub(super) fn codecs(&self) -> Result<Vec<ffi::Guid>, EncodeError> {
        let mut count = 0;
        nvenc(NvencCall::GetEncodeGuids, unsafe {
            (self.api().get_encode_guid_count)(self.handle, &raw mut count)
        })?;

        let mut guids = vec![ffi::Guid::default(); count as usize];
        let mut written = 0;
        nvenc(NvencCall::GetEncodeGuids, unsafe {
            (self.api().get_encode_guids)(self.handle, guids.as_mut_ptr(), count, &raw mut written)
        })?;
        guids.truncate(written as usize);

        Ok(guids)
    }

    pub(super) fn input_formats(&self, codec: ffi::Guid) -> Result<Vec<u32>, EncodeError> {
        let mut count = 0;
        nvenc(NvencCall::GetInputFormats, unsafe {
            (self.api().get_input_format_count)(self.handle, codec, &raw mut count)
        })?;

        let mut formats = vec![0_u32; count as usize];
        let mut written = 0;
        nvenc(NvencCall::GetInputFormats, unsafe {
            (self.api().get_input_formats)(
                self.handle,
                codec,
                formats.as_mut_ptr(),
                count,
                &raw mut written,
            )
        })?;
        formats.truncate(written as usize);

        Ok(formats)
    }

    pub(super) fn caps(&self, codec: ffi::Guid, query: u32) -> Result<i32, EncodeError> {
        let mut params = ffi::CapsParam::zeroed();
        params.version = ffi::CAPS_PARAM_VER;
        params.caps_to_query = query;

        let mut value: c_int = 0;
        nvenc(NvencCall::GetEncodeCaps, unsafe {
            (self.api().get_encode_caps)(self.handle, codec, &raw mut params, &raw mut value)
        })?;

        Ok(value)
    }

    pub(super) fn preset_config(
        &self,
        codec: ffi::Guid,
        preset: ffi::Guid,
        tuning: u32,
    ) -> Result<ffi::Config, EncodeError> {
        let mut preset_config = Box::new(ffi::PresetConfig::zeroed());
        preset_config.version = ffi::PRESET_CONFIG_VER;
        preset_config.preset_cfg.version = ffi::CONFIG_VER;

        nvenc(NvencCall::GetEncodePresetConfigEx, unsafe {
            (self.api().get_encode_preset_config_ex)(
                self.handle,
                codec,
                preset,
                tuning,
                &raw mut *preset_config,
            )
        })?;

        Ok(preset_config.preset_cfg)
    }

    pub(super) fn initialize(&self, params: &mut ffi::InitializeParams) -> Result<(), EncodeError> {
        nvenc(NvencCall::InitializeEncoder, unsafe {
            (self.api().initialize_encoder)(self.handle, params)
        })
    }

    pub(super) fn reconfigure(
        &self,
        params: &mut ffi::ReconfigureParams,
    ) -> Result<(), EncodeError> {
        nvenc(NvencCall::ReconfigureEncoder, unsafe {
            (self.api().reconfigure_encoder)(self.handle, params)
        })
    }

    pub(super) fn encode(&self, params: &mut ffi::PicParams) -> Result<(), EncodeError> {
        nvenc(NvencCall::EncodePicture, unsafe {
            (self.api().encode_picture)(self.handle, params)
        })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _current = self.context.enter();
        unsafe { (self.api().destroy_encoder)(self.handle) };
    }
}

pub(super) struct InputBuffer {
    encoder: Rc<Encoder>,
    buffer: *mut c_void,
    pub(super) format: u32,
}

impl InputBuffer {
    pub(super) fn create(
        encoder: &Rc<Encoder>,
        resolution: Resolution,
        format: u32,
    ) -> Result<Self, EncodeError> {
        let mut params = ffi::CreateInputBuffer::zeroed();
        params.version = ffi::CREATE_INPUT_BUFFER_VER;
        params.width = resolution.width;
        params.height = resolution.height;
        params.memory_heap = ffi::MEMORY_HEAP_AUTOSELECT;
        params.buffer_fmt = format;

        nvenc(NvencCall::CreateInputBuffer, unsafe {
            (encoder.api().create_input_buffer)(encoder.handle, &raw mut params)
        })?;

        Ok(Self {
            encoder: Rc::clone(encoder),
            buffer: params.input_buffer,
            format,
        })
    }

    pub(super) fn pointer(&self) -> *mut c_void {
        self.buffer
    }

    pub(super) fn write(
        &mut self,
        rows: usize,
        fill: impl FnOnce(&mut [u8], usize),
    ) -> Result<u32, EncodeError> {
        let mut params = ffi::LockInputBuffer::zeroed();
        params.version = ffi::LOCK_INPUT_BUFFER_VER;
        params.input_buffer = self.buffer;

        let api = self.encoder.api();
        nvenc(NvencCall::LockInputBuffer, unsafe {
            (api.lock_input_buffer)(self.encoder.handle, &raw mut params)
        })?;

        let pitch = params.pitch;
        let data = unsafe {
            std::slice::from_raw_parts_mut(
                params.buffer_data_ptr.cast::<u8>(),
                pitch as usize * rows,
            )
        };
        fill(data, pitch as usize);
        unsafe { (api.unlock_input_buffer)(self.encoder.handle, self.buffer) };

        Ok(pitch)
    }
}

impl Drop for InputBuffer {
    fn drop(&mut self) {
        unsafe { (self.encoder.api().destroy_input_buffer)(self.encoder.handle, self.buffer) };
    }
}

pub(super) struct Bitstream {
    encoder: Rc<Encoder>,
    buffer: *mut c_void,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Coded {
    pub data: Vec<u8>,
    pub picture_type: u32,
}

impl Bitstream {
    pub(super) fn create(encoder: &Rc<Encoder>) -> Result<Self, EncodeError> {
        let mut params = ffi::CreateBitstreamBuffer::zeroed();
        params.version = ffi::CREATE_BITSTREAM_BUFFER_VER;
        params.memory_heap = ffi::MEMORY_HEAP_AUTOSELECT;

        nvenc(NvencCall::CreateBitstreamBuffer, unsafe {
            (encoder.api().create_bitstream_buffer)(encoder.handle, &raw mut params)
        })?;

        Ok(Self {
            encoder: Rc::clone(encoder),
            buffer: params.bitstream_buffer,
        })
    }

    pub(super) fn pointer(&self) -> *mut c_void {
        self.buffer
    }

    pub(super) fn read(&self) -> Result<Coded, EncodeError> {
        let mut params = ffi::LockBitstream::zeroed();
        params.version = ffi::LOCK_BITSTREAM_VER;
        params.output_bitstream = self.buffer;

        let api = self.encoder.api();
        nvenc(NvencCall::LockBitstream, unsafe {
            (api.lock_bitstream)(self.encoder.handle, &raw mut params)
        })?;

        let data = unsafe {
            std::slice::from_raw_parts(
                params.bitstream_buffer_ptr.cast::<u8>(),
                params.bitstream_size_in_bytes as usize,
            )
        }
        .to_vec();
        unsafe { (api.unlock_bitstream)(self.encoder.handle, self.buffer) };

        Ok(Coded {
            data,
            picture_type: params.picture_type,
        })
    }
}

impl Drop for Bitstream {
    fn drop(&mut self) {
        unsafe { (self.encoder.api().destroy_bitstream_buffer)(self.encoder.handle, self.buffer) };
    }
}

pub(super) struct ExternalBuffer {
    context: Rc<CudaContext>,
    memory: ffi::CuExternalMemory,
    pointer: ffi::CuDevicePtr,
}

impl ExternalBuffer {
    pub(super) fn import(
        context: &Rc<CudaContext>,
        fd: OwnedFd,
        size: u64,
    ) -> Result<Self, EncodeError> {
        let api = &context.driver.cuda;
        let raw = fd.into_raw_fd();

        let mut handle = ffi::ExternalMemoryHandleDesc::zeroed();
        handle.handle_type = ffi::CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD;
        handle.handle.fd = raw;
        handle.size = size;

        let mut memory: ffi::CuExternalMemory = ptr::null_mut();
        let imported = cuda(CudaCall::ImportExternalMemory, unsafe {
            (api.import_external_memory)(&raw mut memory, &raw const handle)
        });
        if let Err(error) = imported {
            drop(unsafe { OwnedFd::from_raw_fd(raw) });
            return Err(error);
        }

        let mut buffer = ffi::ExternalMemoryBufferDesc::zeroed();
        buffer.size = size;

        let mut pointer: ffi::CuDevicePtr = 0;
        let mapped = cuda(CudaCall::ExternalMemoryGetMappedBuffer, unsafe {
            (api.external_memory_get_mapped_buffer)(&raw mut pointer, memory, &raw const buffer)
        });
        if let Err(error) = mapped {
            unsafe { (api.destroy_external_memory)(memory) };
            return Err(error);
        }

        Ok(Self {
            context: Rc::clone(context),
            memory,
            pointer,
        })
    }

    pub(super) fn pointer(&self) -> ffi::CuDevicePtr {
        self.pointer
    }
}

impl Drop for ExternalBuffer {
    fn drop(&mut self) {
        let api = &self.context.driver.cuda;
        let _current = self.context.enter();
        unsafe {
            (api.mem_free)(self.pointer);
            (api.destroy_external_memory)(self.memory);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DevicePlane {
    pub pointer: ffi::CuDevicePtr,
    pub resolution: Resolution,
    pub pitch: u32,
    pub format: u32,
}

pub(super) struct Registration {
    encoder: Rc<Encoder>,
    resource: *mut c_void,
}

pub(super) struct Mapped<'a> {
    registration: &'a Registration,
    pub(super) input: *mut c_void,
    pub(super) format: u32,
}

impl Registration {
    pub(super) fn register(encoder: &Rc<Encoder>, plane: DevicePlane) -> Result<Self, EncodeError> {
        let mut params = ffi::RegisterResource::zeroed();
        params.version = ffi::REGISTER_RESOURCE_VER;
        params.resource_type = ffi::INPUT_RESOURCE_TYPE_CUDADEVICEPTR;
        params.width = plane.resolution.width;
        params.height = plane.resolution.height;
        params.pitch = plane.pitch;
        params.resource_to_register = plane.pointer as *mut c_void;
        params.buffer_format = plane.format;
        params.buffer_usage = ffi::BUFFER_USAGE_INPUT_IMAGE;

        nvenc(NvencCall::RegisterResource, unsafe {
            (encoder.api().register_resource)(encoder.handle, &raw mut params)
        })?;

        Ok(Self {
            encoder: Rc::clone(encoder),
            resource: params.registered_resource,
        })
    }

    pub(super) fn map(&self) -> Result<Mapped<'_>, EncodeError> {
        let mut params = ffi::MapInputResource::zeroed();
        params.version = ffi::MAP_INPUT_RESOURCE_VER;
        params.registered_resource = self.resource;

        nvenc(NvencCall::MapInputResource, unsafe {
            (self.encoder.api().map_input_resource)(self.encoder.handle, &raw mut params)
        })?;

        Ok(Mapped {
            registration: self,
            input: params.mapped_resource,
            format: params.mapped_buffer_fmt,
        })
    }
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        let encoder = &self.registration.encoder;
        unsafe { (encoder.api().unmap_input_resource)(encoder.handle, self.input) };
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        unsafe { (self.encoder.api().unregister_resource)(self.encoder.handle, self.resource) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISSING: LibraryNames = LibraryNames {
        cuda: "librusplay-absent-cuda.so.1",
        nvenc: "librusplay-absent-nvidia-encode.so.1",
    };

    #[test]
    fn a_missing_cuda_library_is_named() {
        let Err(error) = Driver::load(MISSING) else {
            panic!("an absent library loaded");
        };

        assert!(matches!(
            error,
            EncodeError::LibraryNotFound { library, .. } if library == MISSING.cuda
        ));
    }

    #[test]
    fn packed_api_versions_split_into_major_and_minor() {
        assert_eq!(
            api_version_of(ffi::MAX_SUPPORTED_VERSION),
            ApiVersion {
                major: 12,
                minor: 2
            }
        );
        assert_eq!(
            api_version_of(0xb1),
            ApiVersion {
                major: 11,
                minor: 1
            }
        );
        assert!(api_version_of(0xb1) < REQUIRED_API);
    }

    #[test]
    fn failing_calls_carry_their_status() {
        let error = nvenc(NvencCall::EncodePicture, 8).unwrap_err();
        let cuda_error = cuda(CudaCall::ImportExternalMemory, 801).unwrap_err();

        assert_eq!(
            error.to_string(),
            "nvEncEncodePicture failed with invalid parameter (8)"
        );
        assert_eq!(
            cuda_error.to_string(),
            "cuImportExternalMemory failed with not supported (801)"
        );
        assert!(nvenc(NvencCall::EncodePicture, ffi::SUCCESS).is_ok());
    }
}
