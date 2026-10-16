(module
  (func (export "f") (param $x i32) (param $y i32) (result i32)
    (if (result i32) (i32.gt_u (local.get $x) (local.get $y))
      (then (local.get $x))
      (else (local.get $y)))))
