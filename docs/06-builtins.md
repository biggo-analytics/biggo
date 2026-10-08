# built-in reference

รายการฟังก์ชันสำเร็จรูปทั้งหมดของ biggo จัดตามหมวด รายละเอียดเชิงอธิบายอยู่ในหน้าอื่น
หน้านี้ไว้เปิดหาเร็ว ๆ

สัญลักษณ์ในตาราง: *number* คือ `int` `float` หรือ `decimal`; *moment* คือ `date` หรือ `datetime`;
*any* คือชนิดใดก็ได้ที่เป็น column ได้ ทุกฟังก์ชันของค่าเดี่ยวส่งต่อ null: ถ้า argument เป็น null
ผลลัพธ์เป็น null (ยกเว้น `is_null`)

built-in ถูกเรียกได้อย่างเดียว ใช้เป็นค่าไม่ได้ (`map(xs, upper)` ไม่ได้ ต้องเขียน
`map(xs, fn(s) { upper(s) })`) และตั้งชื่อฟังก์ชันของตัวเองซ้ำกับ built-in ไม่ได้

## ฟังก์ชันของค่าเดี่ยว

ใช้ได้ทั้งกับค่าทั่วไปและใน expression ของ column (ซึ่งคำนวณทั้ง column ในคราวเดียว)

### null

| ฟังก์ชัน | ผลลัพธ์ |
| --- | --- |
| `is_null(x)` | `true` ถ้า `x` เป็น null — ได้ `bool` ที่ไม่เป็น null เสมอ |
| `a ?? b` | (ตัวดำเนินการ) `a` ถ้าไม่เป็น null ไม่อย่างนั้น `b` |

### ตัวเลข

| ฟังก์ชัน | รับ | ได้ | ความหมาย |
| --- | --- | --- | --- |
| `abs(x)` | number | ชนิดเดิม | ค่าสัมบูรณ์ |
| `round(x)` | number | `float` (`decimal` ถ้ารับ `decimal`) | ปัดเป็นจำนวนเต็ม ครึ่งปัดออกจากศูนย์ |
| `round(x, digits)` | number, `int` | เหมือนข้างบน | ปัดให้เหลือ `digits` ตำแหน่ง; ค่าลบปัดหลักหน้าจุด |
| `floor(x)` | number | `float` | ปัดลง |
| `ceil(x)` | number | `float` | ปัดขึ้น |
| `sqrt(x)` | number | `float` | รากที่สอง (`NaN` ถ้า `x` ติดลบ) |

```biggo
print(abs(-3), abs(-2.5), abs(-1.5d))
print(round(2.5), round(-2.5), round(3.14159, 2), round(1234.5, -2), round(2.345d, 2))
print(floor(2.7), ceil(2.1), floor(-2.7), sqrt(2), sqrt(-1))
```

```text output
3 2.5 1.5
3.0 -3.0 3.14 1200.0 2.35
2.0 3.0 -3.0 1.4142135623730951 NaN
```

`digits` ของ `round` ต้องเป็นค่าของโปรแกรม ไม่ใช่ column

### string

| ฟังก์ชัน | ได้ | ความหมาย |
| --- | --- | --- |
| `length(s)` | `int` | จำนวนอักขระ (Unicode code point) |
| `lower(s)` `upper(s)` | `string` | ตัวพิมพ์เล็ก / ใหญ่ |
| `trim(s)` | `string` | ตัดช่องว่างหัวท้าย |
| `contains(s, part)` | `bool` | `s` มี `part` อยู่ข้างใน |
| `starts_with(s, part)` | `bool` | `s` ขึ้นต้นด้วย `part` |
| `ends_with(s, part)` | `bool` | `s` ลงท้ายด้วย `part` |
| `a + b` | `string` | (ตัวดำเนินการ) ต่อ string |

```biggo
print(length("biggo"), length("ภาษาไทย"), upper("Abc"), lower("Abc"), trim("  a b  ") + "|")
print(contains("biggo", "gg"), starts_with("biggo", "big"), ends_with("biggo", "x"))
```

```text output
5 7 ABC abc a b|
true true false
```

การค้นหาเป็นแบบตรงตัว แยกตัวพิมพ์เล็ก/ใหญ่ ยังไม่มี regular expression, การตัด substring หรือการแทนที่

### วันที่และเวลา

| ฟังก์ชัน | รับ | ได้ | ความหมาย |
| --- | --- | --- | --- |
| `year(d)` `month(d)` `day(d)` | moment | `int` | ปี, เดือน (1–12), วันของเดือน |
| `hour(t)` `minute(t)` `second(t)` | moment | `int` | ชั่วโมง (0–23), นาที, วินาที; ของ `date` คือ 0 |
| `days(n)` `hours(n)` `minutes(n)` `seconds(n)` | `int` หรือ `float` | `duration` | ช่วงเวลายาว `n` หน่วย |
| `total_seconds(d)` | `duration` | `float` | ความยาวเป็นวินาที |

```biggo
let t = @2026-12-25T18:30:45
print(year(t), month(t), day(t), hour(t), minute(t), second(t), hour(@2026-12-25))
print(days(2), hours(1.5), minutes(90), seconds(0.25), total_seconds(days(1)))
print(t + days(7), t - @2026-01-01, total_seconds(t - @2026-12-25) / 3600)
```

```text output
2026 12 25 18 30 45 0
2d 00:00:00 01:30:00 01:30:00 00:00:00.25 86400.0
2027-01-01T18:30:45 358d 18:30:45 18.5125
```

### การแปลงชนิด

| ฟังก์ชัน | รับ | ได้ |
| --- | --- | --- |
| `to_int(x)` | `int` `float` `decimal` `string` `bool` | `int` (ตัดเศษทิ้ง) |
| `to_float(x)` | `int` `float` `decimal` `string` | `float` |
| `to_decimal(x)` | `int` `float` `decimal` `string` | `decimal` |
| `to_string(x)` | any | `string` |
| `to_date(x)` | `string` `date` `datetime` | `date` |
| `to_datetime(x)` | `string` `date` `datetime` | `datetime` |

string ที่แปลงไม่ได้ทำให้โปรแกรมหยุดด้วย error รายละเอียดและตัวอย่างอยู่ใน
[ระบบ type](03-types.md#การแปลงชนิดด้วยฟังก์ชัน)

## aggregate

ใช้ภายใน `agg(...)`, `pivot(...)` และ (ยกเว้นสี่ตัวสุดท้าย) ภายใน `window(...)`
ย่อค่าของกลุ่มเหลือค่าเดียว ข้าม null

| ฟังก์ชัน | รับ | ได้ |
| --- | --- | --- |
| `count()` | — | `int` จำนวนแถว |
| `count(x)` | any | `int` จำนวนค่าที่ไม่เป็น null |
| `count_distinct(x)` | any | `int` จำนวนค่าที่ต่างกัน |
| `sum(x)` | number, `duration` | ชนิดเดิม |
| `mean(x)` | number | `float` |
| `median(x)` | number | `float` |
| `stddev(x)` | number | `float?` ส่วนเบี่ยงเบนมาตรฐานของตัวอย่าง; null ถ้ามีไม่ถึง 2 ค่า |
| `min(x)` `max(x)` | any ยกเว้น `bool` | ชนิดเดิม |
| `first(x)` `last(x)` | any | ชนิดเดิม ค่าของแถวแรก / สุดท้ายของกลุ่ม |
| `corr(y, x)` | number, number | `float?` สหสัมพันธ์ของ Pearson |
| `cov(y, x)` | number, number | `float?` ความแปรปรวนร่วมของตัวอย่าง |
| `slope(y, x)` | number, number | `float?` ความชันของเส้นกำลังสองน้อยที่สุด |
| `intercept(y, x)` | number, number | `float?` จุดตัดแกน y ของเส้นเดียวกัน |

```biggo
let t = from_rows([
  { g: "a", x: 1, y: 2.0 },
  { g: "a", x: 2, y: 4.5 },
  { g: "a", x: 4, y: 7.5 },
  { g: "b", x: 5, y: null },
])
t
  |> group(g)
  |> agg(
    n = count(),
    ys = count(y),
    total = sum(x),
    avg = mean(y),
    mid = median(x),
    spread = round(stddev(x), 3),
    low = min(y),
    start = first(x),
    r = round(corr(y, x), 3),
    line = round(slope(y, x), 3),
  )
  |> print()
```

```text output
+---+---+----+-------+-------------------+-----+--------+------+-------+-------+-------+
| g | n | ys | total | avg               | mid | spread | low  | start | r     | line  |
+---+---+----+-------+-------------------+-----+--------+------+-------+-------+-------+
| a | 3 | 3  | 7     | 4.666666666666667 | 2.0 | 1.528  | 2.0  | 1     | 0.991 | 1.786 |
| b | 1 | 0  | 5     | null              | 5.0 | null   | null | 5     | null  | null  |
+---+---+----+-------+-------------------+-----+--------+------+-------+-------+-------+
```

รายละเอียด: [group และ agg](04-tables.md#สรุปเป็นกลุ่ม-group-และ-agg), [สถิติ](04-tables.md#สถิติ)

## window function

ใช้ภายใน `window(...)` เท่านั้น ให้ค่าหนึ่งค่าต่อแถว

| ฟังก์ชัน | ได้ | ความหมาย |
| --- | --- | --- |
| `row_number()` | `int` | ลำดับของแถวใน partition เริ่มที่ 1 |
| `rank()` | `int` | อันดับตาม `order`; ค่าเท่ากันได้อันดับเท่ากัน |
| `lag(x)`, `lag(x, n)` | ชนิดของ `x` (nullable) | ค่าในแถวก่อนหน้า `n` แถว |
| `lead(x)`, `lead(x, n)` | ชนิดของ `x` (nullable) | ค่าในแถวถัดไป `n` แถว |
| `cumsum(x)` | ชนิดของ `x` | ผลรวมสะสม (`int` หรือ `float`) |
| `moving_avg(x, n)` | `float` | ค่าเฉลี่ยของ `n` แถวล่าสุด |
| aggregate ใด ๆ | ตามตัวมัน | ค่าของทั้ง partition |

รายละเอียด: [window](04-tables.md#window)

## operation ของตาราง

ทุกตัวรับตารางเป็น argument แรก จึงใช้กับ `|>` ได้ และคืนตาราง (ยกเว้นที่ระบุ)

| operation | ผลลัพธ์ |
| --- | --- |
| `where(t, cond)` | แถวที่ `cond` เป็นจริง |
| `select(t, a, b = expr, ...)` | เฉพาะ column ที่ระบุหรือคำนวณ |
| `drop(t, a, ...)` | ทุก column ยกเว้นที่ระบุ |
| `rename(t, new = old, ...)` | เปลี่ยนชื่อ column |
| `derive(t, c = expr, ...)` | เพิ่มหรือแทนที่ column |
| `sort(t, key, desc(key), asc(key), ...)` | เรียงแถว null อยู่ท้าย |
| `take(t, n)` | `n` แถวแรก |
| `skip(t, n)` | ข้าม `n` แถวแรก |
| `distinct(t)`, `distinct(t, a, ...)` | แถวหรือค่าที่ไม่ซ้ำ |
| `group(t, a, k = expr, ...)` | ตารางที่จัดกลุ่มแล้ว ส่งต่อให้ `agg` หรือ `pivot` |
| `agg(t, name = aggregate, ...)` | หนึ่งแถวต่อกลุ่ม |
| `join(a, b, on = k, how = "inner")` | จับคู่แถวของสองตาราง; `how`: `inner` `left` `right` `full` `semi` `anti` |
| `window(t, by = k, order = k, name = fn, ...)` | เพิ่ม column ที่คำนวณจากแถวข้างเคียง |
| `union(a, b, ...)` | ต่อแถวของหลายตาราง |
| `pivot(t, column, [values], aggregate)` | ค่าของ `column` กลายเป็น column |
| `unpivot(t, a, b, names = "name", values = "value")` | column กลายเป็นแถว |
| `explode(t, column)`, `explode(t, column, sep)` | แยก string ที่มีหลายค่าเป็นหลายแถว |
| `collect(t)` | รันแล้วเก็บผลไว้ในหน่วยความจำ |
| `count(t)` | จำนวนแถว (`int`) |

`desc` และ `asc` ไม่ใช่ฟังก์ชัน เป็นเครื่องหมายกำกับ key ของ `sort` และ `order` ของ `window`

รายละเอียด: [การทำงานกับตาราง](04-tables.md)

## อ่านและเขียนข้อมูล

| ฟังก์ชัน | ความหมาย |
| --- | --- |
| `read_csv<T>(path)` | ตารางจากไฟล์ CSV |
| `read_parquet<T>(path)` | ตารางจากไฟล์ Parquet |
| `read_json<T>(path)` | ตารางจากไฟล์ JSON Lines |
| `read_sql<T>(path, query)` | ผลของ query บนฐานข้อมูล SQLite |
| `write_csv(t, path)` | เขียนตารางเป็น CSV |
| `write_parquet(t, path)` | เขียนตารางเป็น Parquet |
| `write_json(t, path)` | เขียนตารางเป็น JSON Lines |
| `write_sql(t, path, name)` | เขียนตารางลงฐานข้อมูล SQLite ชื่อตาราง `name` |

`T` คือชนิดของแถว เช่น `{ id: int, name: string? }` รายละเอียด: [แหล่งข้อมูล](05-data-sources.md)

## list และ map

| ฟังก์ชัน | ได้ | ความหมาย |
| --- | --- | --- |
| `len(x)` | `int` | จำนวนสมาชิกของ list หรือจำนวน key ของ map |
| `range(n)` | `list<int>` | `0` ถึง `n − 1` |
| `range(a, b)` | `list<int>` | `a` ถึง `b − 1` |
| `map(xs, f)` | `list<U>` | `f(x)` ของแต่ละสมาชิก |
| `filter(xs, f)` | `list<T>` | สมาชิกที่ `f(x)` เป็น `true` |
| `fold(xs, start, f)` | ชนิดของ `start` | สะสมค่าด้วย `f(acc, x)` |
| `each(xs, f)` | — | เรียก `f(x)` ทีละตัว ไม่มีค่า |
| `keys(m)` | `list<K>` | key ตามลำดับที่ถูกใส่ |
| `values(m)` | `list<V>` | value ตามลำดับเดียวกัน |
| `put(m, k, v)` | `map<K, V>` | map ใหม่ที่ `k` มีค่า `v` |
| `has_key(m, k)` | `bool` | map มี key `k` |
| `xs[i]`, `m[k]`, `xs + ys` | | (ตัวดำเนินการ) index และต่อ list |
| `to_rows(t)` | `list<{...}>` | แถวของตารางเป็น list ของ record |
| `from_rows(xs)`, `from_rows<T>(xs)` | ตาราง | ตารางจาก list ของ record |

```biggo
let m = { "a": 1, "b": 2 }
print(len([1, 2, 3]), len(m), range(3), range(2, 5))
print(map([1, 2], fn(x) { x * 10 }), filter([1, 2, 3], fn(x) { x != 2 }))
print(fold(["a", "b"], "", fn(s, x) { s + x }), keys(m), values(m))
print(put(m, "c", 3), has_key(m, "c"), m["a"], m["zz"])
print(to_rows(from_rows([{ id: 1 }, { id: 2 }])))
```

```text output
3 2 [0, 1, 2] [2, 3, 4]
[10, 20] [1, 3]
ab ["a", "b"] [1, 2]
{"a": 1, "b": 2, "c": 3} false 1 null
[{id: 1}, {id: 2}]
```

รายละเอียด: [list](02-language.md#list), [map](02-language.md#map),
[ตารางกับ list ของ record](04-tables.md#ตารางกับ-list-ของ-record)

## สรุปและสถิติ

| ฟังก์ชัน | ได้ | ความหมาย |
| --- | --- | --- |
| `describe(t)` | ตาราง | สถิติสรุปของทุก column |
| `histogram(t, column)`, `histogram(t, column, bins = n)` | ตาราง | จำนวนค่าในแต่ละช่วง |
| `linreg(t, y, x)` | `{ slope, intercept, r2 }` | เส้นถดถอยเชิงเส้นของ `y` บน `x` |

รายละเอียด: [สถิติ](04-tables.md#สถิติ)

## แสดงผลและตรวจสอบ

| ฟังก์ชัน | ความหมาย |
| --- | --- |
| `print(a, b, ...)` | พิมพ์ค่าคั่นด้วยช่องว่าง แล้วขึ้นบรรทัดใหม่; ตารางถูกรันและพิมพ์ 50 แถวแรก |
| `explain(t)` | พิมพ์แผนของ query ทั้งก่อนและหลังปรับ โดยไม่รัน |
| `assert(cond)`, `assert(cond, message)` | หยุดโปรแกรมด้วย error ถ้า `cond` ไม่เป็น `true` |
| `assert_eq(a, b)` | หยุดโปรแกรมถ้าสองค่าไม่เท่ากัน; เทียบ list record map และตารางได้ |

```biggo
print("total:", 42, [1, 2], { ok: true })
assert(1 + 1 == 2, "เลขคณิตพัง")
assert_eq(map([1, 2], fn(n) { n * 2 }), [2, 4])
assert_eq(from_rows([{ id: 1 }]) |> derive(twice = id * 2), from_rows([{ id: 1, twice: 2 }]))
explain(from_rows([{ id: 1 }, { id: 2 }]) |> where(id > 1) |> select(id))
```

```text output
total: 42 [1, 2] {ok: true}
plan:
  Project: id
    Filter: id > 1
      Table: 2 rows
optimized plan:
  Filter: id > 1
    Table: 2 rows
```

`print` แสดง string และวันที่แบบเปล่า ๆ เมื่อเป็นค่าระดับบนสุด และแสดงแบบ literal
(มี `"` หรือ `@`) เมื่ออยู่ใน list, record หรือ map รายละเอียดของ `assert`:
[biggo test](07-tools.md#biggo-test)
