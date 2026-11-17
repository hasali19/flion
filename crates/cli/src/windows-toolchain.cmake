# Toolchain for cross compiling plugins for Windows from a non-Windows host, using clang-cl and
# lld-link with a Windows SDK splatted by xwin (passed in via FLION_WINDOWS_SDK).

set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR AMD64)

set(CMAKE_C_COMPILER clang-cl)
set(CMAKE_CXX_COMPILER clang-cl)
set(CMAKE_C_COMPILER_TARGET x86_64-pc-windows-msvc)
set(CMAKE_CXX_COMPILER_TARGET x86_64-pc-windows-msvc)
set(CMAKE_LINKER lld-link)
set(CMAKE_RC_COMPILER llvm-rc)
set(CMAKE_MT llvm-mt)

# xwin doesn't provide the debug CRT, so compiler checks must be built in release mode.
set(CMAKE_TRY_COMPILE_CONFIGURATION Release)

# Make the SDK path available to try_compile projects.
list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES FLION_WINDOWS_SDK)

if(NOT FLION_WINDOWS_SDK)
    message(FATAL_ERROR "FLION_WINDOWS_SDK must be set when cross compiling for Windows")
endif()

set(_sdk "${FLION_WINDOWS_SDK}")

set(_compile_flags "")
foreach(_dir crt/include sdk/include/ucrt sdk/include/um sdk/include/shared sdk/include/winrt)
    string(APPEND _compile_flags " /imsvc \"${_sdk}/${_dir}\"")
endforeach()

set(_link_flags "")
foreach(_dir crt/lib/x86_64 sdk/lib/um/x86_64 sdk/lib/ucrt/x86_64)
    string(APPEND _link_flags " /libpath:\"${_sdk}/${_dir}\"")
endforeach()

set(CMAKE_C_FLAGS_INIT "${_compile_flags}")
set(CMAKE_CXX_FLAGS_INIT "${_compile_flags}")
set(CMAKE_EXE_LINKER_FLAGS_INIT "${_link_flags}")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "${_link_flags}")
set(CMAKE_MODULE_LINKER_FLAGS_INIT "${_link_flags}")

set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
