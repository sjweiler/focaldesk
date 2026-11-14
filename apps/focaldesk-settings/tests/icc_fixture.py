"""Generate a private RGB display ICC with the same system LittleCMS library."""
import ctypes
import ctypes.util


def write_srgb(path, version=4.3):
    library = ctypes.CDLL(ctypes.util.find_library('lcms2'))
    library.cmsCreate_sRGBProfile.restype = ctypes.c_void_p
    library.cmsSetProfileVersion.argtypes = [ctypes.c_void_p, ctypes.c_double]
    library.cmsSaveProfileToMem.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
    library.cmsCloseProfile.argtypes = [ctypes.c_void_p]
    profile = library.cmsCreate_sRGBProfile()
    assert profile
    try:
        library.cmsSetProfileVersion(profile, version)
        size = ctypes.c_uint32()
        assert library.cmsSaveProfileToMem(profile, None, ctypes.byref(size))
        buffer = ctypes.create_string_buffer(size.value)
        assert library.cmsSaveProfileToMem(profile, buffer, ctypes.byref(size))
        path.write_bytes(buffer.raw[:size.value])
    finally:
        library.cmsCloseProfile(profile)
