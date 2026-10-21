/*
 * linux-hello: a tiny static Linux/aarch64 program with no milkOS knowledge.
 *
 * Freestanding C (no libc): raw Linux syscalls through `svc #0` (number in x8, args in
 * x0..x5, result in x0, -errno on failure), entry `_start` with the Linux initial stack
 * (argc, argv, envp, auxv at sp). Linked at Linux's default 0x400000 as an ET_EXEC.
 * Built by tests/linux/build.sh with the host's bare-metal aarch64-elf-gcc.
 *
 * Exercises: write, getpid, brk, mmap/munmap, openat/read/close (/etc/hostname),
 * exit_group, plus the stack layout (argc/argv/auxv AT_PAGESZ). Prints one line per
 * check and "linux-hello: ALL OK" at the end; exit status = number of failures.
 */

typedef unsigned long u64;
typedef long i64;

#define SYS_openat 56
#define SYS_close 57
#define SYS_read 63
#define SYS_write 64
#define SYS_exit_group 94
#define SYS_getpid 172
#define SYS_brk 214
#define SYS_munmap 215
#define SYS_mmap 222

#define AT_FDCWD -100
#define O_RDONLY 0
#define PROT_READ 1
#define PROT_WRITE 2
#define MAP_PRIVATE 2
#define MAP_ANONYMOUS 0x20
#define AT_NULL 0
#define AT_PAGESZ 6

static i64 sys6(i64 n, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
    register i64 x8 __asm__("x8") = n;
    register i64 x0 __asm__("x0") = a;
    register i64 x1 __asm__("x1") = b;
    register i64 x2 __asm__("x2") = c;
    register i64 x3 __asm__("x3") = d;
    register i64 x4 __asm__("x4") = e;
    register i64 x5 __asm__("x5") = f;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5)
                     : "memory");
    return x0;
}
#define sys3(n, a, b, c) sys6(n, (i64)(a), (i64)(b), (i64)(c), 0, 0, 0)

/* gcc may emit calls to these even with -ffreestanding. */
void *memset(void *d, int c, u64 n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}
void *memcpy(void *d, const void *s, u64 n) {
    unsigned char *p = d;
    const unsigned char *q = s;
    while (n--) *p++ = *q++;
    return d;
}

static u64 slen(const char *s) {
    u64 n = 0;
    while (s[n]) n++;
    return n;
}
static void puts_(const char *s) { sys3(SYS_write, 1, s, slen(s)); }
static void putn(i64 v) {
    char b[24];
    int i = 23;
    int neg = v < 0;
    u64 u = neg ? (u64)-v : (u64)v;
    b[i] = 0;
    do {
        b[--i] = (char)('0' + u % 10);
        u /= 10;
    } while (u);
    if (neg) b[--i] = '-';
    puts_(b + i);
}
static void puthex(u64 v) {
    char b[19];
    int i = 18;
    b[i] = 0;
    do {
        b[--i] = "0123456789abcdef"[v & 15];
        v >>= 4;
    } while (v);
    b[--i] = 'x';
    b[--i] = '0';
    puts_(b + i);
}

static int failures;
static void check(const char *what, int ok) {
    puts_(ok ? "  ok   " : "  FAIL ");
    puts_(what);
    puts_("\n");
    if (!ok) failures++;
}

int cmain(u64 *sp) {
    i64 argc = (i64)sp[0];
    char **argv = (char **)(sp + 1);
    char **envp = argv + argc + 1;
    char **e = envp;
    while (*e) e++;
    u64 *auxv = (u64 *)(e + 1);
    u64 pagesz = 0;
    for (u64 *a = auxv; a[0] != AT_NULL; a += 2)
        if (a[0] == AT_PAGESZ) pagesz = a[1];

    puts_("linux-hello: static Linux/aarch64 ELF at ");
    puthex((u64)&cmain & ~0xfffUL);
    puts_(", argc ");
    putn(argc);
    puts_(", argv[0] ");
    puts_(argc > 0 ? argv[0] : "(none)");
    puts_("\n");
    check("initial stack: argc >= 1, auxv AT_PAGESZ = 4096", argc >= 1 && pagesz == 4096);

    i64 pid = sys3(SYS_getpid, 0, 0, 0);
    puts_("  getpid -> ");
    putn(pid);
    puts_("\n");
    check("getpid > 0", pid > 0);

    /* brk: query, grow by 64 KiB, touch every page, shrink back. */
    i64 b0 = sys3(SYS_brk, 0, 0, 0);
    i64 b1 = sys3(SYS_brk, b0 + 0x10000, 0, 0);
    int brk_ok = b0 > 0 && b1 == b0 + 0x10000;
    if (brk_ok) {
        volatile unsigned char *p = (unsigned char *)b0;
        for (int i = 0; i < 0x10000; i += 4096) p[i] = (unsigned char)i;
        for (int i = 0; i < 0x10000; i += 4096) brk_ok &= p[i] == (unsigned char)i;
        brk_ok &= sys3(SYS_brk, b0, 0, 0) == b0;
    }
    check("brk: grow 64 KiB, write, read back, shrink", brk_ok);

    /* mmap: 64 KiB anonymous read/write, zero-filled, writable; munmap. */
    i64 m = sys6(SYS_mmap, 0, 0x10000, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS,
                 -1, 0);
    int mmap_ok = m > 0 && (m & 0xfff) == 0;
    if (mmap_ok) {
        u64 *q = (u64 *)m;
        mmap_ok &= q[0] == 0 && q[0x10000 / 8 - 1] == 0;
        q[0] = 0x1122334455667788UL;
        q[0x10000 / 8 - 1] = 42;
        mmap_ok &= q[0] == 0x1122334455667788UL && q[0x10000 / 8 - 1] == 42;
        mmap_ok &= sys3(SYS_munmap, m, 0x10000, 0) == 0;
    }
    check("mmap: 64 KiB anonymous, zeroed, writable, munmap", mmap_ok);

    /* openat + read + close: /etc/hostname. */
    i64 fd = sys6(SYS_openat, AT_FDCWD, (i64) "/etc/hostname", O_RDONLY, 0, 0, 0);
    char host[64];
    i64 n = fd >= 0 ? sys3(SYS_read, fd, host, sizeof host - 1) : -1;
    if (n > 0 && host[n - 1] == '\n') n--;
    host[n > 0 ? n : 0] = 0;
    puts_("  /etc/hostname -> \"");
    puts_(host);
    puts_("\"\n");
    check("openat/read/close /etc/hostname", fd >= 0 && n > 0 && sys3(SYS_close, fd, 0, 0) == 0);

    /* Errors come back as -errno. */
    check("openat of a missing file -> -ENOENT",
          sys6(SYS_openat, AT_FDCWD, (i64) "/nonexistent", O_RDONLY, 0, 0, 0) == -2);

    puts_(failures ? "linux-hello: FAILED\n" : "linux-hello: ALL OK\n");
    return failures;
}

/* Linux entry: sp -> argc. Keep sp 16-byte aligned, clear the frame pointer. */
__asm__(".text\n"
        ".global _start\n"
        "_start:\n"
        "    mov x29, xzr\n"
        "    mov x30, xzr\n"
        "    mov x0, sp\n"
        "    bl cmain\n"
        "    mov x8, #94\n" /* exit_group(status) */
        "    svc #0\n"
        "    b .\n");
