## Send

```rust
...
        let ts0 = rdtscp();
        payload.timestamp = ts0;
        tx.send(&payload);
        let ts1 = rdtscp();
...
        pub fn send(&mut self, payload: &T) -> u64 {
            let next_position = self.position.wrapping_add(1) % BUFFER_LEN;

            let seq_no = self.seq_no;
            let next_seq_no = seq_no.wrapping_add(1);

            let next_slot = &mut self.buffer[next_position];
            next_slot.state.store(true, next_seq_no, Ordering::SeqCst);

            let slot = &mut self.buffer[self.position];
            (*slot.payload).clone_from(payload);
            slot.state.store(false, seq_no, Ordering::SeqCst);

            self.position = next_position;
            self.seq_no = next_seq_no;
            seq_no
        }
```

### Payload: 512 bytes

```gas
        rdtscp

        #NO_APP
        leaq    (%rsi,%rsi,4), %r12
        leal    1(%rsi), %ecx
        andl    $4095, %ecx
        leaq    1(%r14), %rsi
        movq    %rcx, 96(%rsp)
        leaq    (%rcx,%rcx,4), %rcx
        shll    $7, %ecx
        movq    %rsi, 88(%rsp)
        movabsq $-9223372036854775808, %rdi
        orq     %rdi, %rsi
        xchgq   %rsi, (%r15,%rcx)
        shlq    $32, %rdx
        movl    %eax, %ebx
        orq     %rdx, %rbx
        shlq    $7, %r12
        movq    %rbx, 128(%r15,%r12)
        leaq    (%r15,%r12), %rdi
        addq    $136, %rdi
        movl    $504, %edx
        leaq    112(%rsp), %rsi
        vzeroupper
        callq   *memcpy@GOTPCREL(%rip)
        xchgq   %r14, (%r15,%r12)
        #APP

        rdtscp
```

### Payload: 256 bytes

```gas
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
        vmovups 288(%rsp), %ymm0
        vmovups %ymm0, 328(%rbx,%rax)
        vmovups 312(%rsp), %ymm0
        vmovups %ymm0, 352(%rbx,%rax)
        xchgq   %rsi, (%rbx,%rax)
        #APP

        rdtscp

```

## Recv

```rust
...
        let ts0 = rdtscp();
        let (seq_no, ts1) = {
            // This path performs a full payload copy.
            // In practice, the optimizer will only copy the bytes that are actually
            // used. In this example, only the `timestamp` field (8 bytes) is copied
            // out of the payload, not the entire payload buffer.
            let (seq_no, payload) = rx.recv();
            (seq_no, payload.timestamp)
        }
        let ts2 = rdtscp();
...
        pub fn recv(&mut self) -> (u64, &T) {
            loop {
                let seq_no = self.load_state();
                let slot = &self.buffer[self.position];
                self.recv_slot.clone_from(&slot.payload);
                let (_, seq_no2) = self.try_load_state();
                if seq_no2 == seq_no {
                    self.position = self.position.wrapping_add(1) % BUFFER_LEN;
                    self.seq_no = seq_no.wrapping_add(1);
                    return (seq_no, &self.recv_slot);
                }
            }
        }



```

```gas
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
