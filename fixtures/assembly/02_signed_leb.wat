(module
  (func (export "f") (param $x i32) (param $y i32) (result i32)
    (i32.xor (i32.add (local.get $x) (i32.const 128)) (i32.const -1))))
