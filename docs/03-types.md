# ระบบ type

biggo ตรวจ type ทั้งโปรแกรมก่อนรัน (static typing) ทุก expression มี type ที่รู้ตอน compile
รวมถึงตาราง ซึ่ง type ของมันคือรายชื่อ column พร้อมชนิด ผลคือ

- พิมพ์ชื่อ column ผิด เอา string ไปบวกเลข หรือลืมจัดการ null รู้ตั้งแต่ก่อนอ่านข้อมูล
- engine รู้ชนิดของทุก column ล่วงหน้า จึงเลือกวิธีคำนวณที่เร็วที่สุดได้โดยไม่ต้องตรวจตอนรัน

ส่วนใหญ่ไม่ต้องเขียน type เอง: compiler อนุมานให้จากค่า ที่ต้องเขียนคือ parameter ของฟังก์ชัน
และชนิดของแถวในไฟล์ข้อมูล

## type ทั้งหมด

| type | ค่า | เป็น column ได้ |
| --- | --- | --- |
| `int` | จำนวนเต็ม 64 บิต | ✓ |
| `float` | ทศนิยม 64 บิต | ✓ |
| `bool` | `true` / `false` | ✓ |
| `string` | ข้อความ UTF-8 | ✓ |
| `date` | วันที่ | ✓ |
| `datetime` | วันที่และเวลา ละเอียดถึงไมโครวินาที ไม่มี time zone | ✓ |
| `duration` | ช่วงเวลา ละเอียดถึงไมโครวินาที | ✓ |
| `decimal` | เลขทศนิยมแม่นยำ 6 ตำแหน่ง รวม 38 หลัก | ✓ |
| `T?` | ค่าชนิด `T` หรือ null | ✓ |
| `list<T>` | ลำดับของค่าชนิด `T` | |
| `map<K, V>` | คู่ key–value | |
| `{ a: T, b: U }` | record: ค่าที่มี field ตามชื่อ | |
| `table<{ a: T, ... }>` | ตารางที่มี column ตามนั้น | |
| `fn(T, U) -> R` | ฟังก์ชัน | |

นอกจากนี้มี type สองตัวที่เขียนในโปรแกรมไม่ได้แต่เห็นใน error message: `unit` (type ของ expression
ที่ไม่มีค่า เช่น `print(...)`) และ *grouped table* (ผลของ `group` ซึ่งต้องส่งต่อให้ `agg`)

## type alias

`type ชื่อ = type` ตั้งชื่อให้ type เพื่อใช้ซ้ำ ประกาศได้ที่ระดับบนสุดของไฟล์เท่านั้น

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
type Sales = table<Sale>
type Score = float?

fn top(sales: Sales, n: int) -> Sales {
  sales |> sort(desc(qty)) |> take(n)
}

let sales = read_csv<Sale>("data/sales.csv")
print(top(sales, 2))
```

```text output
+------------+--------+---------+-----+-------+
| date       | region | product | qty | price |
+------------+--------+---------+-----+-------+
| 2026-03-01 | south  | widget  | 12  | 2.5   |
| 2026-01-05 | north  | widget  | 10  | 2.5   |
+------------+--------+---------+-----+-------+
```

alias เป็นแค่ชื่อเรียก: `Sale` กับ `{ date: date, ... }` ที่เขียนเต็มเป็น type เดียวกัน
type ของ record ที่ใช้เป็นชนิดของแถว (ใน `read_csv<...>` หรือ `table<...>`) ต้องมีแต่ field ที่เป็น
column ได้

## ตัวเลข: int, float, decimal

```biggo
print(9223372036854775807, -9223372036854775807 - 1)   // ขอบเขตของ int
print(0.1 + 0.2, 1e308 * 10, 0 / 0)                    // float: มี inf และ NaN
print(0.1d + 0.2d, 1d / 3, 2d / 3)                     // decimal: แม่นยำ ปัดที่ตำแหน่งที่ 6
```

```text output
9223372036854775807 -9223372036854775808
0.30000000000000004 inf NaN
0.3 0.333333 0.666667
```

| | `int` | `float` | `decimal` |
| --- | --- | --- | --- |
| เก็บเป็น | จำนวนเต็ม 64 บิต | IEEE 754 64 บิต | จำนวนเต็ม 128 บิต × 10⁻⁶ |
| ความแม่นยำ | แม่นยำ | ประมาณ 15–17 หลัก | แม่นยำ 6 ตำแหน่งหลังจุด |
| เมื่อเกินขอบเขต | error `integer overflow` | `inf` | error `decimal overflow` |
| หารด้วยศูนย์ | `/` ได้ `inf`, `%` เป็น error | `inf` หรือ `NaN` | error `division by zero` |
| เหมาะกับ | จำนวนนับ, id | การวัด, สถิติ | เงิน |

ใช้ `decimal` เมื่อผลรวมต้องตรงทุกสตางค์ ใช้ `float` เมื่อความเร็วสำคัญกว่าและค่าคลาดเคลื่อนระดับ
10⁻¹⁵ ยอมรับได้ `float` เร็วกว่าเพราะ CPU คำนวณได้โดยตรง

literal ของ `decimal` มีได้ไม่เกิน 6 ตำแหน่งหลังจุดและ 32 หลักหน้าจุด ผลของ `*` และ `/` ที่มีตำแหน่ง
มากกว่านั้นถูกปัดครึ่งออกจากศูนย์ (round half away from zero) ที่ตำแหน่งที่ 6

## วันที่และเวลา: date, datetime, duration

```biggo
let start = @2026-01-31T22:30:00
let stop = @2026-02-01T01:15:30.5
let took = stop - start                        // datetime - datetime = duration
print(took, total_seconds(took))
print(start + hours(2), start - days(31))      // datetime ± duration = datetime
print(days(1) + hours(12), hours(1) == minutes(60), took > hours(2))
print(year(start), month(start), day(start), hour(start), minute(start), second(stop))

// date ใช้แทน datetime ได้: นับเป็นเวลา 00:00 ของวันนั้น
print(@2026-03-01 - @2026-02-01, @2026-02-01 + days(1), start > @2026-01-31)
print(to_date(start), to_datetime(@2026-05-05))
```

```text output
02:45:30.5 9930.5
2026-02-01T00:30:00 2025-12-31T22:30:00
1d 12:00:00 true true
2026 1 31 22 30 30
28d 00:00:00 2026-02-02T00:00:00 true
2026-01-31 2026-05-05T00:00:00
```

- `date` คือวันในปฏิทิน ปี 0000 ถึง 9999
- `datetime` คือวันและเวลาแบบไม่มี time zone ("เวลาที่เห็นบนนาฬิกา") ละเอียดถึงไมโครวินาที
- `duration` คือช่วงเวลา เป็นลบได้ พิมพ์เป็น `HH:MM:SS` นำหน้าด้วย `Nd` เมื่อเกินหนึ่งวัน

การคำนวณที่ทำได้:

| expression | ผลลัพธ์ |
| --- | --- |
| `datetime - datetime`, `date - date` | `duration` |
| `datetime + duration`, `datetime - duration` | `datetime` |
| `date + duration`, `date - duration` | `datetime` |
| `duration + duration`, `duration - duration` | `duration` |
| เปรียบเทียบ `date`/`datetime` ด้วยกัน, `duration` ด้วยกัน | `bool` |

`duration` คูณหรือหารด้วยตัวเลขโดยตรงไม่ได้ ให้ผ่านจำนวนวินาที:
`seconds(total_seconds(d) * 2)` สร้าง duration ด้วย `days(n)` `hours(n)` `minutes(n)`
`seconds(n)` ซึ่งรับ `int` หรือ `float` ไม่มีหน่วยเดือนหรือปีเพราะความยาวไม่คงที่

## nullable: `T?`

`T?` คือ "ค่าชนิด `T` หรือ null" ทุก type ที่เป็นข้อมูลมี `?` ได้ (ฟังก์ชันและตารางไม่ได้)
รายละเอียดการใช้งานอยู่ใน [ตัวภาษา](02-language.md#null) สรุปกฎของ type:

- ค่าชนิด `T` ใช้ในที่ที่ต้องการ `T?` ได้เสมอ กลับกันไม่ได้: ต้องแกะด้วย `??` ก่อน
- การคำนวณกับ `T?` ได้ผลเป็น type ที่มี `?`
- `a ?? b` ได้ type ของ `b` (ถ้า `b` ไม่มี `?` ผลก็ไม่มี `?`)
- `is_null(x)` ได้ `bool` ที่ไม่มี `?` เสมอ
- เงื่อนไขของ `if`, `where` ที่เป็น `bool?`: `if` ไม่รับ ส่วน `where` นับ null เป็น "ไม่ผ่าน"

```biggo
let maybe: int? = 5
let sure: int = maybe ?? 0
let doubled = maybe * 2          // int?
print(sure, doubled, is_null(doubled))
```

```text output
5 10 false
```

## list, map, record

```biggo
let numbers: list<int> = [1, 2, 3]
let scores: map<string, float> = { "ann": 9.5, "bo": 7 }
let user: { name: string, tags: list<string> } = { name: "ann", tags: ["a"] }
let nested = [{ id: 1, at: [@2026-01-01] }]
print(numbers, scores, user, nested)
```

```text output
[1, 2, 3] {"ann": 9.5, "bo": 7.0} {name: "ann", tags: ["a"]} [{id: 1, at: [@2026-01-01]}]
```

- สมาชิกของ list, value ของ map และ field ของ record เป็นข้อมูลชนิดใดก็ได้ ซ้อนกันได้ไม่จำกัด
  แต่เก็บฟังก์ชันหรือตารางไม่ได้
- key ของ map เป็น `string`, `int`, `bool` หรือ `date`
- record สอง type เป็นชนิดเดียวกันเมื่อ field ชื่อเดียวกัน ลำดับเดียวกัน type เดียวกัน
- `[]` (list ว่าง) และ `{}` ในฐานะ map ว่างไม่มี type ของตัวเอง ต้องมีบริบทบอก เช่น
  `let xs: list<int> = []`

## type ของตาราง

type ของตารางคือ schema ของมัน: ชื่อ ชนิด และลำดับของ column
ทุก operation ของตารางคำนวณ type ของผลลัพธ์ตอน compile

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")

fn units(t: table<{ qty: int }>) -> int {
  to_rows(t |> agg(total = sum(qty)))[0].total
}

// ส่งตารางที่มี column ตรงกับที่ฟังก์ชันประกาศ
print(units(sales |> select(qty)))
```

```text output
45
```

ตารางที่ส่งเข้าฟังก์ชันต้องมี column *ตรงกันพอดี* กับ type ของ parameter (ชื่อ ชนิด ลำดับ)
ไม่ใช่แค่ "มีอย่างน้อยเท่านี้" จึงมักต้อง `select` ก่อนส่ง:

```biggo error
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
fn units(t: table<{ qty: int }>) -> int { count(t) }
print(units(read_csv<Sale>("data/sales.csv")))
```

```text output
error: `units` expects table<{qty: int}> for `t`, found table<{date: date, region: string, product: string, qty: int, price: float?}>
 --> example.bgo:3:13
  |
3 | print(units(read_csv<Sale>("data/sales.csv")))
  |             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

วาง cursor บนตัวแปรใน editor เพื่อดู type ของตาราง ณ จุดนั้นของ pipeline ได้
(ดู [เครื่องมือ](07-tools.md#language-server-และ-editor))

## type ของฟังก์ชัน

`fn(int, string) -> bool` คือฟังก์ชันที่รับ `int` กับ `string` แล้วให้ `bool`
ไม่เขียน `->` คือฟังก์ชันที่ไม่มีค่า ฟังก์ชันสองตัวมี type เดียวกันเมื่อ type ของ parameter และผลลัพธ์ตรงกัน
ชื่อ parameter ไม่เกี่ยว

```biggo
let ops: map<string, int> = { "double": 2, "triple": 3 }
fn scale_by(factor: int) -> fn(int) -> int { fn(n) { n * factor } }
fn apply_all(n: int, f: fn(int) -> int, g: fn(int) -> int) -> int { g(f(n)) }
print(apply_all(5, scale_by(ops["double"] ?? 1), scale_by(ops["triple"] ?? 1)))
```

```text output
30
```

## การแปลงชนิดอัตโนมัติ

compiler แปลงค่าให้เองเฉพาะกรณีที่ **ไม่เสียข้อมูล** มี 4 ทิศ:

| จาก | เป็น | ตัวอย่าง |
| --- | --- | --- |
| `int` | `float` | `1 + 2.5` → `3.5` |
| `int` | `decimal` | `1 + 2.5d` → `3.5d` |
| `decimal` | `float` | `1.5 + 2.5d` → `4.0` |
| `date` | `datetime` | `@2026-01-01 < @2026-01-01T12:00:00` |

และค่าชนิด `T` ใช้เป็น `T?` ได้เสมอ การแปลงเกิดขึ้นในทุกที่ที่ type สองฝั่งต้องเข้ากัน:
ตัวดำเนินการ, argument ของฟังก์ชัน, `let` ที่ระบุ type, branch ของ `if`, arm ของ `match`, สมาชิกของ list

```biggo
let x: float = 1                  // int -> float
let y: decimal = 2                // int -> decimal
let z: float = 2.5d               // decimal -> float
let w: datetime = @2026-01-01     // date -> datetime
print(x, y, z, w)
print([1, 2.5d], [1.5, 2.5d], if true { 1 } else { null })
print(1 + 2.5d, 1.5 + 2.5d, 2.5d / 2, 5 / 2, 7d % 2)
```

```text output
1.0 2 2.5 2026-01-01T00:00:00
[1d, 2.5d] [1.5, 2.5] 1
3.5 4.0 1.25 2.5 1
```

ทิศที่เสียข้อมูลได้ (`float` → `int`, `string` → ตัวเลข ฯลฯ) ต้องเรียกฟังก์ชันแปลงเอง:

```biggo error
let count: int = 2.5
```

```text output
error: expected int, found float
 --> example.bgo:1:18
  |
1 | let count: int = 2.5
  |                  ^^^
```

### list, record, map ที่เขียนออกมาตรง ๆ

ค่าที่ *เขียนออกมาในที่นั้น* (literal) ถูกแปลงทีละส่วนให้เข้ากับ type ที่ต้องการ
ส่วนค่าที่อยู่ในตัวแปรแล้วมี type ตายตัว แปลงได้เฉพาะแบบที่ไม่ต้องแตะข้อมูลข้างใน คือการเพิ่ม `?`

```biggo
type Point = { x: float, y: float }
let a: Point = { x: 1, y: 2 }             // literal: int กลายเป็น float ทีละ field
let ints = [1, 2, 3]
let optional: list<int?> = ints            // ตัวแปร: เพิ่ม ? ได้
print(a, optional)
```

```text output
{x: 1.0, y: 2.0} [1, 2, 3]
```

```biggo error
let ints = [1, 2, 3]
let floats: list<float> = ints             // ตัวแปร: int -> float ทั้ง list ไม่ได้
```

```text output
error: expected list<float>, found list<int>
 --> example.bgo:2:27
  |
2 | let floats: list<float> = ints             // ตัวแปร: int -> float ทั้ง list ไม่ได้
  |                           ^^^^
```

วิธีแปลง list ทั้งก้อนคือ `map(ints, fn(n) { to_float(n) })`

## การแปลงชนิดด้วยฟังก์ชัน

```biggo
print(to_int(3.9), to_int(-3.9), to_int("42"), to_int(true), to_int(19.99d))
print(to_float("1.5"), to_float(2), to_float(2.50d))
print(to_decimal(0.1), to_decimal("2.50"), to_decimal(7))
print(to_string(1.5), to_string(@2026-01-02), to_string(true), to_string(1.50d))
print(to_date("2026-03-04"), to_date(@2026-03-04T23:59:59))
print(to_datetime("2026-03-04T05:06:07"), to_datetime("2026-03-04 05:06:07.25"), to_datetime(@2026-03-04))
```

```text output
3 -3 42 1 19
1.5 2.0 2.5
0.1 2.5 7
1.5 2026-01-02 true 1.5
2026-03-04 2026-03-04
2026-03-04T05:06:07 2026-03-04T05:06:07.25 2026-03-04T00:00:00
```

| ฟังก์ชัน | รับ | หมายเหตุ |
| --- | --- | --- |
| `to_int(x)` | `float` `decimal` `string` `bool` `int` | ตัดเศษทิ้ง (เข้าหาศูนย์) ไม่ปัด; `true` เป็น `1` |
| `to_float(x)` | `int` `decimal` `string` `float` | |
| `to_decimal(x)` | `int` `float` `string` `decimal` | เกิน 6 ตำแหน่งถูกปัด |
| `to_string(x)` | ทุกชนิดที่เป็น column ได้ | ข้อความเดียวกับที่ `print` แสดง |
| `to_date(x)` | `string` `datetime` `date` | string ต้องเป็น `YYYY-MM-DD`; datetime ตัดเวลาทิ้ง |
| `to_datetime(x)` | `string` `date` `datetime` | string เป็น `YYYY-MM-DDTHH:MM:SS` หรือใช้ช่องว่างแทน `T`, มีเศษวินาทีได้ |

ถ้า string แปลงไม่ได้ โปรแกรมหยุดด้วย error ที่บอกค่า (ไม่ได้ null เงียบ ๆ):

```biggo error
print(to_int("12 บาท"))
```

```text output
error: cannot convert '12 บาท' to an int
 --> example.bgo:1:7
  |
1 | print(to_int("12 บาท"))
  |       ^^^^^^^^^^^^^^^^
```

ทุกฟังก์ชันส่งต่อ null: `to_int(x)` เมื่อ `x` เป็น null ได้ null ฟังก์ชันเหล่านี้ใช้กับ column ทั้ง
column ได้เหมือนกัน เช่น `derive(amount = to_decimal(amount_text))`

## การอนุมาน type

compiler อ่านโปรแกรมจากบนลงล่างและรู้ type ของทุก expression จากส่วนประกอบของมัน:

| สิ่งที่เขียน | type ที่ได้ |
| --- | --- |
| `let x = expr` | type ของ `expr` |
| `fn f(a: T) { body }` | ผลลัพธ์คือ type ของ expression สุดท้ายใน `body` |
| `[a, b, c]` | `list` ของ type ที่ทุกตัวแปลงไปหาได้ |
| `if c { a } else { b }`, arm ของ `match` | type ที่ทุก branch แปลงไปหาได้ |
| `fn(x) { ... }` ที่เป็น argument | type ของ `x` มาจาก parameter ที่รับ lambda |
| `t \|> derive(c = expr)` | ตารางเดิมบวก column `c` ที่มี type ของ `expr` |

กรณีที่ compiler ขอให้บอก type เอง:

- `null` ล้วน ๆ: `let x = null` ไม่รู้ว่าเป็น null ของอะไร → `let x: int? = null`
- list ว่างหรือ map ว่าง: `let xs: list<int> = []`
- ฟังก์ชัน recursive: ต้องเขียน `-> T`
- lambda ที่ไม่ได้อยู่ในบริบทที่บอก type ของ parameter

## การเปรียบเทียบและความเท่ากัน

`==` และ `!=` ใช้กับตัวเลข `string` `bool` `date` `datetime` `duration` ส่วน `<` `<=` `>` `>=`
ใช้ได้กับทุกชนิดในนั้นยกเว้น `bool` สองฝั่งต้องเป็นชนิดเดียวกันหรือแปลงหากันได้ตามตารางข้างบน
(`1 == 1.0` ได้ `true`; `1 == "1"` เป็น compile error)

list, record และ map เปรียบเทียบด้วย `==` ไม่ได้ ใน test ใช้ `assert_eq(a, b)` ซึ่งเทียบลึกทั้งโครงสร้าง
รวมถึงตารางสองตาราง ดู [เครื่องมือ](07-tools.md#biggo-test)

string เรียงตามลำดับ byte ของ UTF-8 (ตัวพิมพ์ใหญ่มาก่อนตัวพิมพ์เล็ก; ภาษาไทยเรียงตามรหัสอักขระ
ไม่ใช่ตามพจนานุกรม)
