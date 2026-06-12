(module
  ;; Sim weight-class fixture (fuel-economics spec §5.1): ~60M fuel busy-loop.
  ;; ABI: memory/alloc/run; ignores input; output = 8-byte LE iteration count.
  (memory (export "memory") 1 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "run") (param i32 i32) (result i64)
    (local $i i64)
    (block $done
      (loop $l
        (br_if $done (i64.ge_u (local.get $i) (i64.const 15000000)))
        (local.set $i (i64.add (local.get $i) (i64.const 1)))
        (br $l)))
    (i64.store (i32.const 2048) (local.get $i))
    (i64.or (i64.shl (i64.const 2048) (i64.const 32)) (i64.const 8))))
