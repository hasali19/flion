cmake_minimum_required(VERSION 3.14)

project(FluytFlutterPlugins)

# Compilation settings that should be applied to most targets.
function(APPLY_STANDARD_SETTINGS TARGET)
    target_compile_features(${TARGET} PUBLIC cxx_std_17)
    target_compile_options(${TARGET} PRIVATE /W4 /WX /wd"4100")
    target_compile_options(${TARGET} PRIVATE /EHsc)
    target_compile_definitions(${TARGET} PRIVATE "_HAS_EXCEPTIONS=0")
    target_compile_definitions(${TARGET} PRIVATE "$<$<CONFIG:Debug>:_DEBUG>")

    # Additional settings
    target_compile_options(${TARGET} PRIVATE "$<$<CONFIG:Debug>:/MDd>") # needs to override cargo-rs /MD
    target_compile_definitions(${TARGET} PRIVATE "-D_UNICODE")
endfunction()

add_library(flutter_wrapper_plugin STATIC
    "${CMAKE_CURRENT_SOURCE_DIR}/flutter/flutter-cpp-client-wrapper/cpp_client_wrapper/core_implementations.cc"
    "${CMAKE_CURRENT_SOURCE_DIR}/flutter/flutter-cpp-client-wrapper/cpp_client_wrapper/standard_codec.cc"
    "${CMAKE_CURRENT_SOURCE_DIR}/flutter/flutter-cpp-client-wrapper/cpp_client_wrapper/plugin_registrar.cc"
)
apply_standard_settings(flutter_wrapper_plugin)

target_include_directories(flutter_wrapper_plugin PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/flutter/windows-x64-flutter")

add_library(flutter INTERFACE)
target_include_directories(flutter INTERFACE
    "${CMAKE_CURRENT_SOURCE_DIR}/flutter/windows-x64-flutter"
    "${CMAKE_CURRENT_SOURCE_DIR}/flutter/flutter-cpp-client-wrapper/cpp_client_wrapper/include")

target_link_libraries(flutter INTERFACE "${CMAKE_CURRENT_SOURCE_DIR}/fluyt_plugins_shim.dll.lib")

foreach(plugin ${FLUTTER_PLUGINS})
    add_subdirectory("${plugin}/windows" "plugins/${plugin}")
    list(APPEND PLUGIN_BUNDLED_LIBRARIES $<TARGET_FILE:${plugin}_plugin>)
    list(APPEND PLUGIN_BUNDLED_LIBRARIES ${${plugin}_bundled_libraries})
    install(TARGETS "${plugin}_plugin"
        RUNTIME DESTINATION bin COMPONENT Runtime
        ARCHIVE DESTINATION lib)
endforeach(plugin)

if(PLUGIN_BUNDLED_LIBRARIES)
    install(FILES "${PLUGIN_BUNDLED_LIBRARIES}"
        DESTINATION bin
        COMPONENT Runtime)
endif()
