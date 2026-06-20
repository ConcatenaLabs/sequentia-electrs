# Toolchain env for building the Sequentia explorer (electrs) without system installs.
source "$HOME/.cargo/env"
export LIBCLANG_PATH="$HOME/.local/libclang/usr/lib/llvm-18/lib"
export BINDGEN_EXTRA_CLANG_ARGS="-isystem $HOME/.local/libclang/usr/lib/llvm-18/lib/clang/18/include"
