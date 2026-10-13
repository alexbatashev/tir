// This file was generated with ./utils/scripts/update_checks.py. Do not modify CHECKs manually.

// RUN: fcc compile --march x86_64 --stage ir -o - %S/../Inputs/structs.c | filecheck %s

// CHECK: #data_layout = {cache_line = 64, endianness = "little", stack_alignment = 128, types = {f32 = {abi = 32, size = 32}, f64 = {abi = 64, size = 64}, i1 = {abi = 8, size = 8}, i16 = {abi = 16, size = 16}, i32 = {abi = 32, size = 32}, i64 = {abi = 64, size = 64}, i8 = {abi = 8, size = 8}, p = {abi = 64, size = 64}}}
// CHECK-NEXT: #target_env = {arch = "x86_64", features = ["x86", "x86_64", "sse", "sse2"]}
// CHECK-EMPTY:
// CHECK: module {data_layout = #data_layout, target_env = #target_env} {
// CHECK-NEXT:   %1 = func.func @read(%12299: !ptr.p) -> !i32 {
// CHECK-NEXT:     %2 = ptr.alloca {size = 8, align = 8} : !ptr.p
// CHECK-NEXT:     %3 = ptr.alloca {size = 4, align = 4} : !ptr.p
// CHECK-NEXT:     %8 = constant {value = 4} : !i64
// CHECK-NEXT:     %12300 = state.entry_state : !state<memory>
// CHECK-NEXT:     %12301, %12302, %12303 = state.split state(%12300)
// CHECK-NEXT:     %12291 = ptr.store %12299, %2 state(%12301)
// CHECK-NEXT:     %4, %12292 = ptr.load %2 state(%12291) : !ptr.p
// CHECK-NEXT:     %9 = ptr.ptradd %4, %8 : !ptr.p
// CHECK-NEXT:     %6, %12293 = ptr.load %9 state(%12303) : !i32
// CHECK-NEXT:     %12294 = ptr.store %6, %3 state(%12302)
// CHECK-NEXT:     %7, %12298 = ptr.load %3 state(%12294) : !i32
// CHECK-NEXT:     %12304 = state.join state(%12292, %12298, %12293)
// CHECK-NEXT:     -> %7, %12304
// CHECK-NEXT:   }
// CHECK-NEXT:   %4096 = func.func @copy() -> !i32 {
// CHECK-NEXT:     %4097 = ptr.alloca {size = 8, align = 4} : !ptr.p
// CHECK-NEXT:     %4098 = ptr.alloca {size = 8, align = 4} : !ptr.p
// CHECK-NEXT:     %4099 = ptr.alloca {size = 4, align = 4} : !ptr.p
// CHECK-NEXT:     %4101 = constant {value = 37} : !i32
// CHECK-NEXT:     %4105 = constant {value = 4} : !i64
// CHECK-NEXT:     %4106 = ptr.ptradd %4097, %4105 : !ptr.p
// CHECK-NEXT:     %4107 = constant {value = 4} : !i64
// CHECK-NEXT:     %4108 = ptr.ptradd %4098, %4107 : !ptr.p
// CHECK-NEXT:     %8192 = constant {value = 8} : !i64
// CHECK-NEXT:     %16404 = state.entry_state : !state<memory>
// CHECK-NEXT:     %16405, %16406, %16407, %16408 = state.split state(%16404)
// CHECK-NEXT:     %16388 = state.join state(%16405, %16408)
// CHECK-NEXT:     %16389 = ptr.store %4101, %4106 state(%16388)
// CHECK-NEXT:     %16390, %16391 = state.split state(%16389)
// CHECK-NEXT:     %16392 = state.join state(%16391, %16390, %16406)
// CHECK-NEXT:     %16393 = ptr.memcpy %4098, %4097, %8192 state(%16392)
// CHECK-NEXT:     %16394, %16395, %16396 = state.split state(%16393)
// CHECK-NEXT:     %4103, %16397 = ptr.load %4108 state(%16396) : !i32
// CHECK-NEXT:     %16398 = ptr.store %4103, %4099 state(%16407)
// CHECK-NEXT:     %4104, %16403 = ptr.load %4099 state(%16398) : !i32
// CHECK-NEXT:     %16409 = state.join state(%16395, %16397, %16403, %16394)
// CHECK-NEXT:     -> %4104, %16409
// CHECK-NEXT:   }
// CHECK-NEXT:   module_end
// CHECK-NEXT: }
