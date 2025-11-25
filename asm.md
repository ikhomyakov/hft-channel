## Send

### Payload: 408 bytes

```asm
        rdtscp

        #NO_APP
        leaq    (%rsi,%rsi,4), %r12
        leal    1(%rsi), %ecx
        andl    $4095, %ecx
        leaq    1(%r14), %rsi
        movq    %rcx, 88(%rsp)
        leaq    (%rcx,%rcx,4), %rcx
        shll    $7, %ecx
        movq    %rsi, 80(%rsp)
        movabsq $-9223372036854775808, %rdi
        orq     %rdi, %rsi
        movq    72(%rsp), %r13
        xchgq   %rsi, (%r13,%rcx)
        shlq    $32, %rdx
        movl    %eax, %r15d
        orq     %rdx, %r15
        shlq    $7, %r12
        movq    %r15, 128(%r13,%r12)
        leaq    (%r12,%r13), %rdi
        addq    $136, %rdi
        movl    $400, %edx
        leaq    96(%rsp), %rsi
        vzeroupper
        callq   *memcpy@GOTPCREL(%rip)
        xchgq   %r14, (%r13,%r12)
        #APP

        rdtscp
```

### Payload: 208 bytes

```asm
        rdtscp

        #NO_APP
        shlq    $32, %rdx
        movl    %eax, %ebp
        orq     %rdx, %rbp
        leaq    (%rdi,%rdi,2), %rax
        leal    1(%rdi), %ecx
        andl    $4095, %ecx
        leaq    1(%rsi), %rdx
        movq    %rcx, 80(%rsp)
        leaq    (%rcx,%rcx,2), %rcx
        shll    $7, %ecx
        movq    %rdx, 72(%rsp)
        movabsq $-9223372036854775808, %rdi
        orq     %rdi, %rdx
        xchgq   %rdx, (%rbx,%rcx)
        shlq    $7, %rax
        movq    %rbp, 128(%rbx,%rax)
        vmovups 96(%rsp), %ymm0
        vmovups 128(%rsp), %ymm1
        vmovups 160(%rsp), %ymm2
        vmovups 192(%rsp), %ymm3
        vmovups %ymm0, 136(%rbx,%rax)
        vmovups %ymm1, 168(%rbx,%rax)
        vmovups %ymm2, 200(%rbx,%rax)
        vmovups %ymm3, 232(%rbx,%rax)
        vmovups 224(%rsp), %ymm0
        vmovups %ymm0, 264(%rbx,%rax)
        vmovups 256(%rsp), %ymm0
        vmovups %ymm0, 296(%rbx,%rax)
        movq    288(%rsp), %rcx
        movq    %rcx, 328(%rbx,%rax)
        xchgq   %rsi, (%rbx,%rax)
        #APP

        rdtscp
```

## Recv

```asm
        rdtscp

        #NO_APP
        movl    %eax, 64(%rsp)
        movl    %edx, %r15d
        leaq    (,%r13,4), %rax
        addq    %r13, %rax
        shlq    $7, %rax
        addq    %r12, %rax
        .p2align        4
.LBB47_36:
        movq    (%rax), %r12
        testq   %r12, %r12
        js      .LBB47_36
        movq    128(%rax), %rax
        movq    %rax, 48(%rsp)
        movq    %r12, 184(%rsp)
        #APP

        rdtscp
```
