# Install script for directory: /Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream

# Set the install prefix
if(NOT DEFINED CMAKE_INSTALL_PREFIX)
  set(CMAKE_INSTALL_PREFIX "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out")
endif()
string(REGEX REPLACE "/$" "" CMAKE_INSTALL_PREFIX "${CMAKE_INSTALL_PREFIX}")

# Set the install configuration name.
if(NOT DEFINED CMAKE_INSTALL_CONFIG_NAME)
  if(BUILD_TYPE)
    string(REGEX REPLACE "^[^A-Za-z0-9_]+" ""
           CMAKE_INSTALL_CONFIG_NAME "${BUILD_TYPE}")
  else()
    set(CMAKE_INSTALL_CONFIG_NAME "Release")
  endif()
  message(STATUS "Install configuration: \"${CMAKE_INSTALL_CONFIG_NAME}\"")
endif()

# Set the component getting installed.
if(NOT CMAKE_INSTALL_COMPONENT)
  if(COMPONENT)
    message(STATUS "Install component: \"${COMPONENT}\"")
    set(CMAKE_INSTALL_COMPONENT "${COMPONENT}")
  else()
    set(CMAKE_INSTALL_COMPONENT)
  endif()
endif()

# Is this installation the result of a crosscompile?
if(NOT DEFINED CMAKE_CROSSCOMPILING)
  set(CMAKE_CROSSCOMPILING "FALSE")
endif()

# Set path to fallback-tool for dependency-resolution.
if(NOT DEFINED CMAKE_OBJDUMP)
  set(CMAKE_OBJDUMP "/opt/homebrew/opt/llvm/bin/llvm-objdump")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE STATIC_LIBRARY FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/libsnmallocshim-static.a")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-static.a" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-static.a")
    execute_process(COMMAND "/opt/homebrew/opt/llvm/bin/llvm-ranlib" "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-static.a")
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE STATIC_LIBRARY FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/libsnmalloc-new-override.a")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmalloc-new-override.a" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmalloc-new-override.a")
    execute_process(COMMAND "/opt/homebrew/opt/llvm/bin/llvm-ranlib" "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmalloc-new-override.a")
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE SHARED_LIBRARY FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/libsnmallocshim.dylib")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim.dylib" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim.dylib")
    if(CMAKE_INSTALL_DO_STRIP)
      execute_process(COMMAND "/usr/bin/strip" -x "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim.dylib")
    endif()
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE SHARED_LIBRARY FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/libsnmallocshim-checks-memcpy-only.dylib")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks-memcpy-only.dylib" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks-memcpy-only.dylib")
    if(CMAKE_INSTALL_DO_STRIP)
      execute_process(COMMAND "/usr/bin/strip" -x "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks-memcpy-only.dylib")
    endif()
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE SHARED_LIBRARY FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/libsnmallocshim-checks.dylib")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks.dylib" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks.dylib")
    if(CMAKE_INSTALL_DO_STRIP)
      execute_process(COMMAND "/usr/bin/strip" -x "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks.dylib")
    endif()
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE SHARED_LIBRARY FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/libsnmalloc-minimal.dylib")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmalloc-minimal.dylib" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmalloc-minimal.dylib")
    if(CMAKE_INSTALL_DO_STRIP)
      execute_process(COMMAND "/usr/bin/strip" -x "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmalloc-minimal.dylib")
    endif()
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE STATIC_LIBRARY FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/libsnmallocshim-rust.a")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-rust.a" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-rust.a")
    execute_process(COMMAND "/opt/homebrew/opt/llvm/bin/llvm-ranlib" "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-rust.a")
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE STATIC_LIBRARY FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/libsnmallocshim-checks-rust.a")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks-rust.a" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks-rust.a")
    execute_process(COMMAND "/opt/homebrew/opt/llvm/bin/llvm-ranlib" "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsnmallocshim-checks-rust.a")
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/aal")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/backend")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/backend_helpers")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/ds")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/ds_aal")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/ds_core")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/global")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/mem")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/override")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/pal")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE DIRECTORY FILES "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/stl")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc/test" TYPE FILE FILES
    "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/test/measuretime.h"
    "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/test/opt.h"
    "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/test/setup.h"
    "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/test/usage.h"
    "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/test/xoroshiro.h"
    )
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include/snmalloc" TYPE FILE FILES
    "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/snmalloc.h"
    "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/snmalloc_core.h"
    "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snmalloc-sys-0.7.4/upstream/src/snmalloc/snmalloc_front.h"
    )
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/share/snmalloc/snmalloc-config.cmake")
    file(DIFFERENT _cmake_export_file_changed FILES
         "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/share/snmalloc/snmalloc-config.cmake"
         "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/CMakeFiles/Export/43135d38e178c7a1b69df443136c9627/snmalloc-config.cmake")
    if(_cmake_export_file_changed)
      file(GLOB _cmake_old_config_files "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/share/snmalloc/snmalloc-config-*.cmake")
      if(_cmake_old_config_files)
        string(REPLACE ";" ", " _cmake_old_config_files_text "${_cmake_old_config_files}")
        message(STATUS "Old export file \"$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/share/snmalloc/snmalloc-config.cmake\" will be replaced.  Removing files [${_cmake_old_config_files_text}].")
        unset(_cmake_old_config_files_text)
        file(REMOVE ${_cmake_old_config_files})
      endif()
      unset(_cmake_old_config_files)
    endif()
    unset(_cmake_export_file_changed)
  endif()
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/share/snmalloc" TYPE FILE FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/CMakeFiles/Export/43135d38e178c7a1b69df443136c9627/snmalloc-config.cmake")
  if(CMAKE_INSTALL_CONFIG_NAME MATCHES "^([Rr][Ee][Ll][Ee][Aa][Ss][Ee])$")
    file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/share/snmalloc" TYPE FILE FILES "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/CMakeFiles/Export/43135d38e178c7a1b69df443136c9627/snmalloc-config-release.cmake")
  endif()
endif()

string(REPLACE ";" "\n" CMAKE_INSTALL_MANIFEST_CONTENT
       "${CMAKE_INSTALL_MANIFEST_FILES}")
if(CMAKE_INSTALL_LOCAL_ONLY)
  file(WRITE "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/install_local_manifest.txt"
     "${CMAKE_INSTALL_MANIFEST_CONTENT}")
endif()
if(CMAKE_INSTALL_COMPONENT)
  if(CMAKE_INSTALL_COMPONENT MATCHES "^[a-zA-Z0-9_.+-]+$")
    set(CMAKE_INSTALL_MANIFEST "install_manifest_${CMAKE_INSTALL_COMPONENT}.txt")
  else()
    string(MD5 CMAKE_INST_COMP_HASH "${CMAKE_INSTALL_COMPONENT}")
    set(CMAKE_INSTALL_MANIFEST "install_manifest_${CMAKE_INST_COMP_HASH}.txt")
    unset(CMAKE_INST_COMP_HASH)
  endif()
else()
  set(CMAKE_INSTALL_MANIFEST "install_manifest.txt")
endif()

if(NOT CMAKE_INSTALL_LOCAL_ONLY)
  file(WRITE "/Users/matthew/projects/lodestone/crates/lodestone-allocbench/target-snmalloc/release/build/snmalloc-sys-45d94dc1a081f78f/out/build/${CMAKE_INSTALL_MANIFEST}"
     "${CMAKE_INSTALL_MANIFEST_CONTENT}")
endif()
