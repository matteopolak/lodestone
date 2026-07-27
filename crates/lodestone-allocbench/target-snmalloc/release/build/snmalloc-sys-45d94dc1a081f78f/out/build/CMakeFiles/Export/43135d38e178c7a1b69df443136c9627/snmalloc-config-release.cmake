#----------------------------------------------------------------
# Generated CMake target import file for configuration "Release".
#----------------------------------------------------------------

# Commands may need to know the format version.
set(CMAKE_IMPORT_FILE_VERSION 1)

# Import target "snmalloc::snmallocshim-static" for configuration "Release"
set_property(TARGET snmalloc::snmallocshim-static APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(snmalloc::snmallocshim-static PROPERTIES
  IMPORTED_LINK_INTERFACE_LANGUAGES_RELEASE "CXX"
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libsnmallocshim-static.a"
  )

list(APPEND _cmake_import_check_targets snmalloc::snmallocshim-static )
list(APPEND _cmake_import_check_files_for_snmalloc::snmallocshim-static "${_IMPORT_PREFIX}/lib/libsnmallocshim-static.a" )

# Import target "snmalloc::snmalloc-new-override" for configuration "Release"
set_property(TARGET snmalloc::snmalloc-new-override APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(snmalloc::snmalloc-new-override PROPERTIES
  IMPORTED_LINK_INTERFACE_LANGUAGES_RELEASE "CXX"
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libsnmalloc-new-override.a"
  )

list(APPEND _cmake_import_check_targets snmalloc::snmalloc-new-override )
list(APPEND _cmake_import_check_files_for_snmalloc::snmalloc-new-override "${_IMPORT_PREFIX}/lib/libsnmalloc-new-override.a" )

# Import target "snmalloc::snmallocshim" for configuration "Release"
set_property(TARGET snmalloc::snmallocshim APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(snmalloc::snmallocshim PROPERTIES
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libsnmallocshim.dylib"
  IMPORTED_SONAME_RELEASE "@rpath/libsnmallocshim.dylib"
  )

list(APPEND _cmake_import_check_targets snmalloc::snmallocshim )
list(APPEND _cmake_import_check_files_for_snmalloc::snmallocshim "${_IMPORT_PREFIX}/lib/libsnmallocshim.dylib" )

# Import target "snmalloc::snmallocshim-checks-memcpy-only" for configuration "Release"
set_property(TARGET snmalloc::snmallocshim-checks-memcpy-only APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(snmalloc::snmallocshim-checks-memcpy-only PROPERTIES
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libsnmallocshim-checks-memcpy-only.dylib"
  IMPORTED_SONAME_RELEASE "@rpath/libsnmallocshim-checks-memcpy-only.dylib"
  )

list(APPEND _cmake_import_check_targets snmalloc::snmallocshim-checks-memcpy-only )
list(APPEND _cmake_import_check_files_for_snmalloc::snmallocshim-checks-memcpy-only "${_IMPORT_PREFIX}/lib/libsnmallocshim-checks-memcpy-only.dylib" )

# Import target "snmalloc::snmallocshim-checks" for configuration "Release"
set_property(TARGET snmalloc::snmallocshim-checks APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(snmalloc::snmallocshim-checks PROPERTIES
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libsnmallocshim-checks.dylib"
  IMPORTED_SONAME_RELEASE "@rpath/libsnmallocshim-checks.dylib"
  )

list(APPEND _cmake_import_check_targets snmalloc::snmallocshim-checks )
list(APPEND _cmake_import_check_files_for_snmalloc::snmallocshim-checks "${_IMPORT_PREFIX}/lib/libsnmallocshim-checks.dylib" )

# Import target "snmalloc::snmalloc-minimal" for configuration "Release"
set_property(TARGET snmalloc::snmalloc-minimal APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(snmalloc::snmalloc-minimal PROPERTIES
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libsnmalloc-minimal.dylib"
  IMPORTED_SONAME_RELEASE "@rpath/libsnmalloc-minimal.dylib"
  )

list(APPEND _cmake_import_check_targets snmalloc::snmalloc-minimal )
list(APPEND _cmake_import_check_files_for_snmalloc::snmalloc-minimal "${_IMPORT_PREFIX}/lib/libsnmalloc-minimal.dylib" )

# Import target "snmalloc::snmallocshim-rust" for configuration "Release"
set_property(TARGET snmalloc::snmallocshim-rust APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(snmalloc::snmallocshim-rust PROPERTIES
  IMPORTED_LINK_INTERFACE_LANGUAGES_RELEASE "CXX"
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libsnmallocshim-rust.a"
  )

list(APPEND _cmake_import_check_targets snmalloc::snmallocshim-rust )
list(APPEND _cmake_import_check_files_for_snmalloc::snmallocshim-rust "${_IMPORT_PREFIX}/lib/libsnmallocshim-rust.a" )

# Import target "snmalloc::snmallocshim-checks-rust" for configuration "Release"
set_property(TARGET snmalloc::snmallocshim-checks-rust APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(snmalloc::snmallocshim-checks-rust PROPERTIES
  IMPORTED_LINK_INTERFACE_LANGUAGES_RELEASE "CXX"
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libsnmallocshim-checks-rust.a"
  )

list(APPEND _cmake_import_check_targets snmalloc::snmallocshim-checks-rust )
list(APPEND _cmake_import_check_files_for_snmalloc::snmallocshim-checks-rust "${_IMPORT_PREFIX}/lib/libsnmallocshim-checks-rust.a" )

# Commands beyond this point should not need to know the version.
set(CMAKE_IMPORT_FILE_VERSION)
