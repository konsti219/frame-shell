#!/usr/bin/env bash
# Cross-builds the ExecuTorch runtime (XNNPACK) for the headset's eye model: build-executorch.sh <src> <prefix>
set -euo pipefail
src=$1
prefix=$2
build=${ET_BUILD_DIR:-/tmp/et-build}
# Operators of the eye model that XNNPACK doesn't take, from Babble Frame Runtime's tools/build_executorch.sh
ops="aten::alias_copy.out,aten::as_strided_copy.out,aten::index.Tensor_out,aten::native_layer_norm.out"
ops+=",aten::split_with_sizes_copy.out,aten::squeeze_copy.dims_out,aten::view_copy.out,aten::permute_copy.out"
ops+=",aten::clone.out,aten::cat.out,aten::clamp.out,aten::mean.out,aten::add.out,aten::mul.out"
cmake -S "$src" -B "$build" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE=/opt/aarch64-toolchain.cmake \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DPYTHON_EXECUTABLE=python3 \
    -DEXECUTORCH_BUILD_XNNPACK=ON \
    -DEXECUTORCH_XNNPACK_SHARED_WORKSPACE=OFF \
    -DEXECUTORCH_BUILD_EXTENSION_MODULE=ON \
    -DEXECUTORCH_BUILD_EXTENSION_DATA_LOADER=ON \
    -DEXECUTORCH_BUILD_EXTENSION_FLAT_TENSOR=ON \
    -DEXECUTORCH_BUILD_EXTENSION_NAMED_DATA_MAP=ON \
    -DEXECUTORCH_BUILD_EXTENSION_TENSOR=ON \
    -DEXECUTORCH_BUILD_PORTABLE_OPS=ON \
    -DEXECUTORCH_SELECT_OPS_LIST="$ops" \
    -DEXECUTORCH_ENABLE_LOGGING=ON \
    -DEXECUTORCH_BUILD_EXECUTOR_RUNNER=OFF \
    -DEXECUTORCH_BUILD_DEVTOOLS=OFF \
    -DEXECUTORCH_BUILD_PYBIND=OFF
cmake --build "$build" --parallel "$(nproc)"
cmake --install "$build"
