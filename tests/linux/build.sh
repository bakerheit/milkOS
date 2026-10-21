#!/usr/bin/env bash
# Build tests/linux/linux-hello with the host's bare-metal aarch64-elf-gcc (no libc, no
# downloads): a static Linux/aarch64 ET_EXEC linked at 0x400000. The output is committed
# (bin/linux-hello) so the kernel build doesn't need the cross compiler; rerun this after
# editing the source. initramfs.list puts it at /bin/linux-hello.
set -euo pipefail
cd "$(dirname "$0")"
CC=${CC:-aarch64-elf-gcc}
mkdir -p bin
"$CC" -O2 -static -nostdlib -nostartfiles -ffreestanding -fno-builtin -fno-stack-protector \
  -fno-pic -no-pie -mgeneral-regs-only -Wall -Wextra -Werror \
  -Wl,-Ttext-segment=0x400000 -Wl,-e,_start -Wl,--build-id=none -Wl,-z,noexecstack -Wl,-z,max-page-size=4096 \
  -o bin/linux-hello linux-hello.c
"${STRIP:-aarch64-elf-strip}" bin/linux-hello
echo "built $(pwd)/bin/linux-hello ($(wc -c < bin/linux-hello | tr -d ' ') bytes)"
