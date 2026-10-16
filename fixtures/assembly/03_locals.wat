(module
  (func (export "f") (param $x i32) (param $y i32) (result i32)
    (local $z i32)
    (local.set $z (i32.mul (local.get $x) (i32.const 33)))
    (i32.add (local.get $z) (local.get $y))))
