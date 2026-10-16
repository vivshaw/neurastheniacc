(module
  (func $helper (param i32) (result i32)
    (i32.add (local.get 0) (i32.const 1)))
  (func (export "f") (param $x i32) (param $y i32) (result i32)
    (i32.add (call $helper (local.get $x)) (local.get $y))))
