# ตัวภาษา

หน้านี้อธิบายไวยากรณ์และความหมายของภาษา biggo ส่วนที่ไม่เกี่ยวกับตาราง: ค่า ตัวแปร ตัวดำเนินการ
`if` `match` ฟังก์ชัน lambda list record map และ `import` เรื่องตารางอยู่ใน
[การทำงานกับตาราง](04-tables.md) และรายละเอียดของ type อยู่ใน [ระบบ type](03-types.md)

ตัวอย่างทุกอันในเอกสารถูกรันจริงตอน `cargo test` ผลลัพธ์ที่เห็นคือผลลัพธ์จริง

## โครงสร้างของโปรแกรม

โปรแกรมคือลำดับของ *statement* ซึ่งมี 5 แบบ:

| statement | ตัวอย่าง |
| --- | --- |
| `import` | `import "lib/shapes.bgo"` |
| `type` | `type Sale = { region: string, qty: int }` |
| `let` | `let total = 10` |
| `fn` | `fn double(n: int) -> int { n * 2 }` |
| expression | `print(total)` |

โปรแกรมรันจากบนลงล่าง ไม่มีฟังก์ชัน `main`

### การจบ statement

หนึ่ง statement จบที่ท้ายบรรทัด ไม่มี `;` statement จะต่อไปบรรทัดถัดไปเมื่อเห็นได้ชัดว่ายังไม่จบ คือ

- อยู่ในวงเล็บ `(...)` หรือ `[...]` ที่ยังไม่ปิด
- บรรทัดจบด้วยตัวดำเนินการ เช่น `1 +`
- บรรทัดถัดไปขึ้นต้นด้วยตัวดำเนินการ เช่น `|>` `+` `and` `??` `.`

```biggo
let total = 1 +
  2
let longer = [1, 2, 3]
  |> map(fn(n) { n * total })
  |> fold(0, fn(sum, n) { sum + n })
print(total, longer)
```

```text output
3 18
```

ข้อยกเว้นคือ `-` `(` และ `[` ที่ต้นบรรทัดถือว่าเริ่ม statement ใหม่เสมอ เพราะทั้งสามขึ้นต้น expression
ได้ด้วยตัวเอง (`-x`, `(a + b)`, `[1, 2]`)

### comment

`//` ถึงท้ายบรรทัดเป็น comment ไม่มี comment แบบหลายบรรทัด

## ค่าพื้นฐาน

```biggo
print(42, 1_000_000, 3.14, 2.5e3)
print(19.99d)
print("สวัสดี", "tab\there", "quote: \"x\"")
print(true, false, null)
print(@2026-01-31, @2026-01-31T18:30:00)
```

```text output
42 1000000 3.14 2500.0
19.99
สวัสดี tab	here quote: "x"
true false null
2026-01-31 2026-01-31T18:30:00
```

| ชนิด | literal | หมายเหตุ |
| --- | --- | --- |
| `int` | `42`, `1_000_000` | จำนวนเต็ม 64 บิต `_` ใช้คั่นหลักได้ |
| `float` | `3.14`, `2.5e3` | ทศนิยม 64 บิต (IEEE 754) |
| `decimal` | `19.99d`, `5d` | ทศนิยมแม่นยำ 6 ตำแหน่ง ลงท้ายด้วย `d` |
| `string` | `"text"` | UTF-8 escape ที่ใช้ได้: `\n` `\t` `\r` `\0` `\\` `\"` |
| `bool` | `true`, `false` | |
| `date` | `@2026-01-31` | วันที่ตามปฏิทิน ต้องเป็นวันที่มีจริง |
| `datetime` | `@2026-01-31T18:30:00` | วินาทีใส่หรือไม่ก็ได้ มีเศษวินาทีได้ถึง 6 หลัก ไม่มี time zone |
| null | `null` | "ไม่มีค่า" ใช้ได้กับ type ที่ลงท้ายด้วย `?` เท่านั้น |

string เขียนข้ามบรรทัดไม่ได้ ถ้าต้องการขึ้นบรรทัดใหม่ใช้ `\n`

`duration` (ช่วงเวลา) ไม่มี literal สร้างจากฟังก์ชัน `days(n)` `hours(n)` `minutes(n)` `seconds(n)`
หรือจากการลบ datetime สองค่า ดู [ระบบ type](03-types.md#วันที่และเวลา-date-datetime-duration)

## ตัวแปร

`let` ผูกชื่อกับค่า ค่าของตัวแปรเปลี่ยนไม่ได้ (ไม่มีการกำหนดค่าซ้ำ) แต่ประกาศชื่อเดิมซ้ำได้
ซึ่งเป็นตัวแปรตัวใหม่ที่บังตัวเก่า

```biggo
let price = 100
let price = price * 1.07      // ตัวใหม่ คำนวณจากตัวเก่า
let label: string = "total"   // ระบุ type ก็ได้
let nothing: int? = null      // null ต้องบอก type เสมอ
print(label, price, nothing)
```

```text output
total 107.0 null
```

ปกติไม่ต้องเขียน type เพราะ compiler อนุมานจากค่าให้ ต้องเขียนเมื่อค่าบอก type ไม่ครบ เช่น `null` หรือ
list ว่าง `[]`

## ตัวดำเนินการ

เรียงจากผูกแน่นที่สุดไปหลวมที่สุด:

| ลำดับ | ตัวดำเนินการ | ความหมาย | การจัดกลุ่ม |
| --- | --- | --- | --- |
| 1 | `f(x)` `x.name` `x[i]` | เรียกฟังก์ชัน, field, index | ซ้ายไปขวา |
| 2 | `-x` | ค่าลบ | |
| 3 | `*` `/` `%` | คูณ หาร เศษ | ซ้ายไปขวา |
| 4 | `+` `-` | บวก ลบ | ซ้ายไปขวา |
| 5 | `??` | ถ้าซ้ายเป็น null ใช้ขวา | ขวาไปซ้าย |
| 6 | `==` `!=` `<` `<=` `>` `>=` | เปรียบเทียบ | ต่อกันไม่ได้ |
| 7 | `not` | นิเสธ | |
| 8 | `and` | และ | ซ้ายไปขวา |
| 9 | `or` | หรือ | ซ้ายไปขวา |
| 10 | `\|>` | ส่งค่าเข้าฟังก์ชัน | ซ้ายไปขวา |

```biggo
print(1 + 2 * 3, (1 + 2) * 3, -2 * 3, 10 - 4 - 3)
print(7 / 2, 6 / 3, 7 % 3, -7 % 3, 7.5 % 2)
print(not true or true, 1 < 2 and 2 < 3)
print("data" + "base", [1, 2] + [3])
```

```text output
7 9 -6 3
3.5 2.0 1 -1 1.5
true true
database [1, 2, 3]
```

กฎที่ควรรู้:

- **`/` ให้ `float` เสมอ** แม้หารลงตัว (`6 / 3` คือ `2.0`) ถ้าต้องการจำนวนเต็มใช้ `to_int(a / b)`
  ยกเว้นเมื่อตัวตั้งหรือตัวหารเป็น `decimal` ผลจะเป็น `decimal`
- **`int` ล้นแล้วหยุด** ไม่วนกลับเงียบ ๆ: `9223372036854775807 + 1` เป็น runtime error
  `integer overflow` และ `x % 0` เป็น `division by zero` ส่วน `float` หารด้วยศูนย์ได้ `inf`
- **การเปรียบเทียบต่อกันไม่ได้**: `1 < x < 10` เป็น syntax error ให้เขียน `1 < x and x < 10`
- `+` ใช้ต่อ string และต่อ list ได้
- `int` กับ `float` ผสมกันได้ ฝั่ง `int` ถูกแปลงเป็น `float` ให้เอง กฎเต็มอยู่ใน
  [ระบบ type](03-types.md#การแปลงชนิดอัตโนมัติ)
- `and` / `or` ประเมินฝั่งขวาเฉพาะเมื่อจำเป็น

## null

ค่าที่ "อาจไม่มี" มี type ลงท้ายด้วย `?` เช่น `int?` กฎของ null เหมือน SQL: **การคำนวณที่มี null
ได้ผลเป็น null**

```biggo
let n: int? = null
print(n + 1, n > 1, upper("a") + (if is_null(n) { "!" } else { "?" }))
print(n ?? 0, is_null(n), not is_null(n))
// and / or รู้คำตอบได้แม้อีกฝั่งเป็น null
print(false and n > 1, true or n > 1, true and n > 1)
```

```text output
null null A!
0 true false
false true null
```

เครื่องมือสำหรับจัดการ null มีสองอย่าง:

- `a ?? b` — ใช้ `a` ถ้าไม่ใช่ null ไม่อย่างนั้นใช้ `b` (ผลลัพธ์ไม่เป็น null ถ้า `b` ไม่เป็น null)
- `is_null(a)` — ได้ `true`/`false` เสมอ

และมีข้อห้ามสองข้อที่ compiler ตรวจให้:

```biggo error
let n: int? = null
if n > 1 { print("big") }
```

```text output
error: this condition can be null; say what null means, for example with `?? false`
 --> example.bgo:2:4
  |
2 | if n > 1 { print("big") }
  |    ^^^^^
```

เงื่อนไขของ `if` ต้องเป็น `bool` ที่ไม่เป็น null โปรแกรมต้องบอกเองว่า null หมายถึงอะไร เช่น
`if n > 1 ?? false { ... }`

```biggo error
let n: int? = null
print(n == null)
```

```text output
error: comparing with `null` always gives null; use `is_null(...)` to test for null
 --> example.bgo:2:7
  |
2 | print(n == null)
  |       ^^^^^^^^^
```

`x == null` ได้ null เสมอตามกฎข้างบน ซึ่งแทบไม่เคยเป็นสิ่งที่ตั้งใจ จึงเป็น error ตั้งแต่ตอน compile

## if

`if` เป็น expression: มีค่า และค่านั้นคือค่าของ branch ที่ถูกเลือก

```biggo
let qty = 7
let size = if qty == 0 {
  "none"
} else if qty < 5 {
  "small"
} else {
  "large"
}
print(size, if qty > 5 { 1 } else { 2.5 })
```

```text output
large 1.0
```

ทุก branch ต้องมี type ที่เข้ากันได้ (`1` กับ `2.5` รวมเป็น `float`; ค่ากับ `null` รวมเป็น type ที่มี `?`)
`if` ที่ไม่มี `else` ไม่มีค่า ใช้เพื่อผลข้างเคียงอย่างการ `print` เท่านั้น

## match

`match` เทียบค่ากับ pattern ทีละ arm จากบนลงล่าง แล้วให้ค่าของ arm แรกที่ตรง

```biggo
fn size(qty: int) -> string {
  match qty {
    0 => "none"
    1 | 2 | 3 => "small"
    -1 => "returned"
    _ => "large"
  }
}
print(map([0, 2, 50, -1], size))
```

```text output
["none", "small", "large", "returned"]
```

- pattern เป็น literal (ตัวเลข string bool วันที่ `null`) หรือ `_` ซึ่งตรงกับทุกค่า
- `a | b` คือ "ตรงกับ `a` หรือ `b`"
- arm คั่นด้วยการขึ้นบรรทัดใหม่หรือ `,`
- **ต้องครอบคลุมทุกค่า**: ต้องมี arm `_` ปิดท้าย ยกเว้น `bool` ที่มีทั้ง `true` และ `false` ครบแล้ว
- ค่าที่ถูก match คำนวณครั้งเดียว ไม่ว่าจะมีกี่ arm

ค่าที่เป็น null ไม่ตรงกับ literal ใดเลย ตรงเฉพาะ pattern `null` กับ `_`:

```biggo
fn label(score: int?) -> string {
  match score {
    null => "ยังไม่มีคะแนน"
    100 => "เต็ม"
    _ => "มีคะแนน"
  }
}
let missing: int? = null
print(label(missing), label(100), label(55))
print(match true { true => "yes", false => "no" })
```

```text output
ยังไม่มีคะแนน เต็ม มีคะแนน
yes
```

```biggo error
let qty = 3
print(match qty { 1 => "one", 2 => "two" })
```

```text output
error: this `match` does not cover every value; add a `_ => ...` arm
 --> example.bgo:2:13
  |
2 | print(match qty { 1 => "one", 2 => "two" })
  |             ^^^
```

`match` ใช้ใน expression ของ column ได้ด้วย ดู [การทำงานกับตาราง](04-tables.md#expression-ของ-column)

## block

`{ ... }` คือ block: ลำดับของ statement ที่มีขอบเขตตัวแปรของตัวเอง ค่าของ block คือค่าของ
expression สุดท้าย

```biggo
let area = {
  let width = 3
  let height = 4
  width * height
}
print(area)
```

```text output
12
```

ตัวแปรใน block มองไม่เห็นจากข้างนอก ตัวของฟังก์ชัน และ branch ของ `if` ก็คือ block
ภาษานี้ไม่มี `return`: ฟังก์ชันให้ค่าของ expression สุดท้ายในตัวมัน

## ฟังก์ชัน

```biggo
fn area(width: float, height: float) -> float {
  width * height
}

// ไม่เขียน type ของผลลัพธ์ก็ได้ compiler ดูจากตัวฟังก์ชัน
fn show(name: string, size: float) {
  print(name, "มีพื้นที่", size)
}

show("ห้อง", area(3, 4.5))
// ส่ง argument ตามชื่อได้ ตามลำดับใดก็ได้ แต่ต้องอยู่หลัง argument ที่ส่งตามตำแหน่ง
print(area(height = 2, width = 10), area(10, height = 2))
```

```text output
ห้อง มีพื้นที่ 13.5
20.0 20.0
```

- parameter ต้องระบุ type เสมอ
- type ของผลลัพธ์ (`-> T`) ละได้ ยกเว้นฟังก์ชันที่เรียกตัวเอง (recursive) ต้องระบุ
- ฟังก์ชันที่ไม่มีค่า (ลงท้ายด้วย `print` เป็นต้น) ไม่ต้องมี `->`
- `area(3, 4.5)`: `3` เป็น `int` แต่ parameter เป็น `float` จึงถูกแปลงให้เอง

### การมองเห็นชื่อ

ฟังก์ชันระดับบนสุดเรียกได้จากทุกที่ในไฟล์ แม้ประกาศไว้ข้างล่าง แต่ **ฟังก์ชันเห็นเฉพาะตัวแปรที่ประกาศ
ก่อนหน้ามันในไฟล์**

```biggo
let rate = 0.07
print(with_tax(100))        // เรียกก่อนประกาศได้

fn with_tax(amount: float) -> float { amount * (1 + rate) }
```

```text output
107.0
```

ถ้าฟังก์ชันอยู่เหนือตัวแปรที่มันใช้ compiler จะปฏิเสธ:

```biggo error
fn with_tax(amount: float) -> float { amount * (1 + rate) }
let rate = 0.07
```

```text output
error: undefined name `rate`
 --> example.bgo:1:53
  |
1 | fn with_tax(amount: float) -> float { amount * (1 + rate) }
  |                                                     ^^^^
```

กฎนี้กันความผิดพลาดได้เกือบหมด เหลือกรณีเดียวที่ต้องรอถึงตอนรัน คือ *เรียก* ฟังก์ชันก่อนที่ `let`
ของตัวแปรที่มันใช้จะทำงาน:

```biggo error
print(with_tax(100))        // rate ยังไม่มีค่า ณ จุดนี้
let rate = 0.07
fn with_tax(amount: float) -> float { amount * (1 + rate) }
```

```text output
error: `rate` is used before it has a value
 --> example.bgo:3:53
  |
3 | fn with_tax(amount: float) -> float { amount * (1 + rate) }
  |                                                     ^^^^
```

แนวทางที่ปลอดภัย: ประกาศค่าคงที่ไว้บนสุด ตามด้วยฟังก์ชัน แล้วค่อยเป็นโค้ดที่เรียกใช้

### recursion

```biggo
fn factorial(n: int) -> int {
  if n <= 1 { 1 } else { n * factorial(n - 1) }
}
fn fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
print(factorial(20), fib(20))
```

```text output
2432902008176640000 6765
```

การเรียกซ้อนกันได้ลึก 100,000 ชั้น เกินนั้นโปรแกรมหยุดด้วย
`stack overflow: the program recurses too deeply` ภาษานี้ไม่มีลูป `for`/`while`:
งานที่ทำซ้ำใช้ `map` `filter` `fold` `each` บน list (ดูข้างล่าง) หรือ recursion

### ฟังก์ชันภายในฟังก์ชัน

ประกาศ `fn` ใน block ได้ ฟังก์ชันข้างในเห็นตัวแปรของฟังก์ชันข้างนอก (closure)

```biggo
fn total_with_fee(amounts: list<float>, fee: float) -> float {
  fn add_fee(amount: float) -> float { amount + fee }
  fold(map(amounts, add_fee), 0.0, fn(sum, x) { sum + x })
}
print(total_with_fee([10, 20.5], 1))
```

```text output
32.5
```

## lambda และฟังก์ชันในฐานะค่า

`fn(...) { ... }` ที่ไม่มีชื่อคือ lambda: ฟังก์ชันที่เป็นค่า เก็บในตัวแปร ส่งเป็น argument
หรือคืนจากฟังก์ชันอื่นได้

```biggo
let double = fn(x: int) -> int { x * 2 }
print(double(21))

// type ของฟังก์ชันเขียนว่า fn(type ของ parameter) -> type ของผลลัพธ์
fn twice(f: fn(int) -> int, x: int) -> int { f(f(x)) }

// lambda ที่ส่งเป็น argument ไม่ต้องเขียน type: รู้จาก parameter ที่รับมัน
print(twice(double, 3), twice(fn(n) { n + 1 }, 3))

// ฟังก์ชันที่คืนฟังก์ชัน lambda จำตัวแปรรอบตัวมันไว้
fn adder(amount: int) -> fn(int) -> int {
  fn(n) { n + amount }
}
let add5 = adder(5)
print(add5(1), map([1, 2, 3], add5))
```

```text output
42
12 5
6 [6, 7, 8]
```

type ของ parameter ของ lambda ละได้เมื่อบริบทบอกอยู่แล้ว คือเมื่อ lambda

- เป็น argument ของฟังก์ชันที่ parameter มี type เป็นฟังก์ชัน (รวมถึง `map` `filter` `fold` `each`)
- เป็นค่าของ `let` ที่ระบุ type
- เป็นค่าสุดท้ายของฟังก์ชันที่ประกาศ type ของผลลัพธ์เป็นฟังก์ชัน

นอกเหนือจากนั้นต้องเขียนเอง:

```biggo error
let inc = fn(x) { x + 1 }
```

```text output
error: cannot tell the type of `x` here; write it, as in `x: int`
 --> example.bgo:1:14
  |
1 | let inc = fn(x) { x + 1 }
  |              ^
```

ฟังก์ชันที่ประกาศด้วยชื่อก็ใช้เป็นค่าได้ (`map(xs, double)`) แต่ **built-in ใช้เป็นค่าไม่ได้**
ต้องห่อด้วย lambda: `map(names, fn(s) { upper(s) })`

## list

list คือลำดับของค่าชนิดเดียวกัน เขียนใน `[...]` type คือ `list<T>`

```biggo
let primes = [2, 3, 5, 7, 11]
print(primes[0], primes[-1], len(primes))      // index ติดลบนับจากท้าย
print(primes + [13], range(5), range(2, 6))

print(map(primes, fn(p) { p * p }))
print(filter(primes, fn(p) { p % 4 == 3 }))
print(fold(primes, 0, fn(sum, p) { sum + p }))
each(filter(primes, fn(p) { p > 6 }), fn(p) { print("big prime:", p) })

let empty: list<string> = []
print(empty, len(empty), [1, 2.5], [1, null])
```

```text output
2 11 5
[2, 3, 5, 7, 11, 13] [0, 1, 2, 3, 4] [2, 3, 4, 5]
[4, 9, 25, 49, 121]
[3, 7, 11]
28
big prime: 7
big prime: 11
[] 0 [1.0, 2.5] [1, null]
```

| ฟังก์ชัน | ผลลัพธ์ |
| --- | --- |
| `len(xs)` | จำนวนสมาชิก |
| `xs[i]` | สมาชิกตัวที่ `i` นับจาก 0; `-1` คือตัวสุดท้าย; เกินขอบเขตเป็น runtime error |
| `xs + ys` | list ใหม่ที่ต่อกัน |
| `range(n)`, `range(a, b)` | `[0, ..., n-1]`, `[a, ..., b-1]` (สูงสุด 10 ล้านตัว) |
| `map(xs, f)` | list ของ `f(x)` |
| `filter(xs, f)` | เฉพาะตัวที่ `f(x)` เป็น `true` |
| `fold(xs, start, f)` | เริ่มจาก `start` แล้ว `acc = f(acc, x)` ทีละตัว |
| `each(xs, f)` | เรียก `f(x)` ทีละตัวเพื่อผลข้างเคียง ไม่มีค่า |

สมาชิกของ list ต้องเป็นข้อมูล: ตัวเลข string วันที่ record map หรือ list ซ้อนกันได้
แต่เก็บฟังก์ชันหรือตารางไม่ได้ ค่าทุกชนิดเปลี่ยนแปลงไม่ได้หลังสร้าง ฟังก์ชันอย่าง `+` และ `map`
จึงสร้าง list ใหม่เสมอ

`fold` ต้องการ `start` ที่มี type ชัดเจน และฟังก์ชันต้องคืน type เดียวกับ `start`:
ถ้าจะสะสมเป็น `float` ให้เริ่มด้วย `0.0` ไม่ใช่ `0`

## record

record คือค่าที่มี field ตามชื่อ เขียน `{ name: value, ... }` และอ่าน field ด้วย `.`

```biggo
let ann = { name: "Ann", age: 31, tags: ["admin", "dev"] }
print(ann, ann.name, ann.age + 1, ann.tags[0])

type Point = { x: float, y: float }
fn norm(p: Point) -> float { sqrt(p.x * p.x + p.y * p.y) }
print(norm({ x: 3, y: 4 }))

let points: list<Point> = [{ x: 1, y: 2 }, { x: 0.5, y: 0 }]
print(map(points, fn(p) { p.x + p.y }))
```

```text output
{name: "Ann", age: 31, tags: ["admin", "dev"]} Ann 32 admin
5.0
[3.0, 0.5]
```

type ของ record คือรายชื่อ field พร้อม type ตามลำดับ: `{ x: float, y: float }` record สอง type
เป็นชนิดเดียวกันเมื่อ field ชื่อเดียวกัน เรียงเหมือนกัน และ type ตรงกัน record ที่ *เขียนออกมาตรง ๆ*
(`{ x: 3, y: 4 }`) จะถูกแปลงทีละ field ให้เข้ากับ type ที่ต้องการ เช่น `3` กลายเป็น `3.0` ข้างบน

แถวของตารางก็คือ record: `to_rows(table)` ได้ `list` ของ record และ `from_rows(list)` สร้างตารางจาก
list ของ record ดู [การทำงานกับตาราง](04-tables.md#ตารางกับ-list-ของ-record)

## map

map จับคู่ key กับ value เขียน `{ key: value, ... }` โดย key เป็น literal type คือ `map<K, V>`

```biggo
let stock = { "tea": 4, "cake": 0 }
print(stock["tea"], stock["milk"], stock["milk"] ?? 0)   // key ที่ไม่มีได้ null

let more = put(put(stock, "milk", 9), "tea", 5)          // put คืน map ใหม่
print(more, stock)
print(keys(more), values(more), len(more), has_key(more, "milk"))

// map ว่างต้องบอก type
let none: map<string, int> = {}
let counts = fold(["a", "b", "a"], none, fn(seen, word) {
  put(seen, word, (seen[word] ?? 0) + 1)
})
print(counts)
```

```text output
4 null 0
{"tea": 5, "cake": 0, "milk": 9} {"tea": 4, "cake": 0}
["tea", "cake", "milk"] [5, 0, 9] 3 true
{"a": 2, "b": 1}
```

- key เป็น `string` `int` `bool` หรือ `date`; value เป็นข้อมูลชนิดใดก็ได้ (เหมือนสมาชิกของ list)
- `m[key]` ได้ type `V?`: เป็น null เมื่อไม่มี key นั้น ใช้ `??` กำหนดค่าเริ่มต้น
- map จำลำดับที่ key ถูกใส่ครั้งแรก `keys` `values` และการพิมพ์เรียงตามนั้น
- `put` ไม่แก้ map เดิม แต่คืน map ใหม่ (ต้องคัดลอกทั้ง map จึงเหมาะกับ map ขนาดเล็กถึงกลาง)

`{ ... }` ที่ขึ้นต้นด้วย `ชื่อ:` เป็น record, ขึ้นต้นด้วย `literal:` เป็น map นอกนั้นเป็น block
ดังนั้น key ของ map ที่เขียนตรง ๆ ต้องเป็น literal ถ้า key มาจากตัวแปรให้ใช้ `put`

## pipeline

`a |> f(b, c)` มีความหมายเท่ากับ `f(a, b, c)` ทุกประการ: ค่าทางซ้ายเป็น argument แรก
ฝั่งขวาของ `|>` ต้องเป็นการเรียกฟังก์ชัน

```biggo
fn clamp(x: int, low: int, high: int) -> int {
  if x < low { low } else if x > high { high } else { x }
}
print(15 |> clamp(0, 10))
[3, 1, 2]
  |> map(fn(n) { n * 10 })
  |> filter(fn(n) { n > 10 })
  |> print()
```

```text output
10
[30, 20]
```

`|>` ผูกหลวมที่สุด ทั้ง expression ทางซ้ายจึงเป็นค่าที่ถูกส่ง: `1 + 2 |> print()` พิมพ์ `3`

## import

`import "path"` นำ definition ระดับบนสุด (ตัวแปร ฟังก์ชัน type) ของอีกไฟล์มาใช้
path นับจากตำแหน่งของไฟล์ที่เขียน `import`

ไฟล์ [`lib/geometry.bgo`](lib/geometry.bgo):

```biggo check
type Rect = { width: float, height: float }

fn area(rect: Rect) -> float { rect.width * rect.height }

let unit_square: Rect = { width: 1, height: 1 }
```

โปรแกรมที่ใช้:

```biggo
import "lib/geometry.bgo"

let door: Rect = { width: 0.9, height: 2.0 }
print(area(door), area(unit_square))
```

```text output
1.8 1.0
```

- `import` ต้องอยู่บนสุดของไฟล์ ก่อน statement อื่น
- ไฟล์ที่ถูก import ถูก *รัน* หนึ่งครั้ง (statement ระดับบนสุดของมันทำงานจริง รวมถึง `print`)
  ก่อนไฟล์ที่ import มัน ไม่ว่าจะถูก import จากกี่ที่ก็รันครั้งเดียว
- ชื่อทั้งหมดอยู่ในที่เดียวกัน (ไม่มี namespace): ชื่อจากไฟล์ที่ import เข้ามา รวมถึงจากไฟล์ที่
  ไฟล์นั้น import ต่ออีกที ใช้ได้โดยตรง ชื่อที่ประกาศซ้ำทีหลังบังชื่อก่อนหน้า
- path ของไฟล์ข้อมูลในไฟล์ที่ถูก import นับจากตำแหน่งของไฟล์นั้นเอง
- import เป็นวงกลม (A import B, B import A) เป็น error
- `biggo build` รวมไฟล์ที่ import ไว้ใน executable ให้ด้วย

## ชื่อ

ชื่อของตัวแปร ฟังก์ชัน type และ column ขึ้นต้นด้วยตัวอักษรหรือ `_` ตามด้วยตัวอักษร ตัวเลข หรือ `_`
ตัวอักษรคือตัวอักษร Unicode จึงใช้ภาษาไทยได้

```biggo
let ยอดขาย = [120, 80]
let ภาษี = fn(ยอด: int) -> float { ยอด * 0.07 }
print(map(ยอดขาย, ภาษี))

// ชื่อที่มีช่องว่างหรือชนกับ keyword ครอบด้วย backtick
let `unit price` = 2.5
print(`unit price` * 4)
```

```text output
[8.4, 5.6000000000000005]
10.0
```

คำสงวน: `let` `fn` `type` `import` `if` `else` `match` `and` `or` `not` `true` `false` `null`

ชื่อของ built-in (เช่น `print` `sum` `map` `where`) ใช้ตั้งชื่อฟังก์ชันไม่ได้ แต่ใช้เป็นชื่อตัวแปรหรือ
column ได้ เพราะ built-in ถูกเรียกในรูป `name(...)` เสมอ ซึ่งแยกออกจากตัวแปรได้

## error ตอนรัน

ข้อผิดพลาดส่วนใหญ่ถูกจับตอน compile ที่เหลือซึ่งรู้ได้ตอนรันเท่านั้นจะหยุดโปรแกรมพร้อมบอกตำแหน่ง:

```biggo error
fn bucket(id: int, buckets: int) -> int {
  id % buckets
}
print("before")
print(bucket(7, 0))
```

```text output
before
error: division by zero
 --> example.bgo:2:3
  |
2 |   id % buckets
  |   ^^^^^^^^^^^^
```

runtime error ของภาษา (ไม่นับที่มาจากข้อมูล เช่น ไฟล์ไม่มี หรือค่าใน CSV ผิดชนิด):

| error | เกิดเมื่อ |
| --- | --- |
| `integer overflow` | ผลของ `+` `-` `*` หรือ `-x` ของ `int` เกิน 64 บิต |
| `division by zero` | `%` ด้วยศูนย์ หรือ `/` `%` ของ `decimal` ด้วยศูนย์ |
| `decimal overflow` | ผลของ `decimal` เกิน 38 หลัก |
| `index ... is out of range` | index ของ list เกินขอบเขต |
| `stack overflow` | เรียกฟังก์ชันซ้อนกันเกิน 100,000 ชั้น |
| `assertion failed` | `assert` / `assert_eq` ไม่ผ่าน |
| `... is used before it has a value` | อ่านตัวแปรระดับบนสุดก่อนที่ `let` ของมันจะรัน |

โปรแกรมที่จบด้วย error ออกด้วย exit code 1 สิ่งที่พิมพ์ไปก่อนหน้ายังอยู่ครบ
