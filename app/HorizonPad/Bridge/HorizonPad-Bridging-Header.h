//
//  HorizonPad-Bridging-Header.h
//  HorizonPad
//
//  Exposes the Rust engine's C ABI to Swift. The static library is produced by
//  scripts/build-ios.sh and linked through the `hz_ffi.xcframework` reference
//  in project.yml.
//
//  `#include`, not `#import`: clang-cl (which is what a Windows Swift toolchain
//  uses under the hood) reads `#import` as an MSVC type-library import and
//  fails. `#include` behaves identically inside a guard and works everywhere.
//

#include "hz_ffi.h"