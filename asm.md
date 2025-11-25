## Writer

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


## Reader

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
