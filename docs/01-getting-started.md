# เริ่มต้นใช้งาน biggo

biggo เป็นภาษาสำหรับงานวิเคราะห์ข้อมูล โปรแกรมหนึ่งไฟล์ (`.bgo`) อ่านข้อมูลเข้ามาเป็นตาราง
แปลงตารางด้วย pipeline (`|>`) แล้วพิมพ์หรือเขียนผลลัพธ์ออกไป ตัวภาษาตรวจ type ทั้งหมดก่อนรัน
รวมถึงชื่อและชนิดของ column ทุกตัว ดังนั้นพิมพ์ชื่อ column ผิดจะรู้ตั้งแต่ยังไม่ได้อ่านข้อมูลสักแถว

หน้านี้พาไปตั้งแต่ build จนรันโปรแกรมแรก

## สิ่งที่ต้องมี

- Rust รุ่นปัจจุบัน ติดตั้งได้จาก <https://rustup.rs> (โปรเจกต์ใช้ edition 2024 และพัฒนา/ทดสอบกับ
  Rust 1.99)
- C compiler ของระบบ (ใช้ build SQLite ที่ฝังมากับ biggo) บน macOS คือ Xcode Command Line Tools

ไม่ต้องติดตั้งฐานข้อมูลหรือ library อื่นเพิ่ม ทุกอย่างถูก link เข้าไปใน executable ตัวเดียว

## Build

```sh
git clone https://github.com/biggo-analytics/biggo.git && cd biggo
cargo build --release
```

ได้ไฟล์ `target/release/biggo` ขนาดราว 22 MB คัดลอกไปไว้ใน `PATH` ได้เลย:

```sh
cp target/release/biggo ~/.local/bin/
biggo version
```

ต้องใช้ `--release` เสมอเมื่อจะวัดความเร็วหรือใช้งานจริง build แบบ debug ช้ากว่าหลายสิบเท่า

## โปรแกรมแรก

สร้างไฟล์ `hello.bgo`:

```biggo
let name = "biggo"
print("hello,", name)
print(1 + 2 * 3, 7 / 2, [1, 2, 3])
```

แล้วรัน `biggo run hello.bgo` จะได้

```text output
hello, biggo
7 3.5 [1, 2, 3]
```

`print` รับค่ากี่ตัวก็ได้ คั่นด้วยช่องว่างตอนพิมพ์ และ `/` ให้ผลเป็นทศนิยมเสมอ (`7 / 2` คือ `3.5`)

## โปรแกรมวิเคราะห์ข้อมูลแรก

ตัวอย่างในเอกสารชุดนี้ใช้ไฟล์ [`data/sales.csv`](data/sales.csv) ซึ่งมี 10 แถว:

```text
date,region,product,qty,price
2026-01-05,north,widget,10,2.5
2026-01-17,north,gadget,3,10.0
2026-01-20,south,widget,0,2.5
2026-02-02,south,gadget,5,
...
```

โปรแกรมนี้หายอดขายรวมของแต่ละภูมิภาค:

```biggo
// บอกว่าแต่ละแถวของไฟล์มี column อะไร ชนิดอะไร
type Sale = { date: date, region: string, product: string, qty: int, price: float? }

let sales = read_csv<Sale>("data/sales.csv")

sales
  |> where(qty > 0)
  |> derive(revenue = qty * (price ?? 0.0))
  |> group(region)
  |> agg(total = sum(revenue), orders = count())
  |> sort(desc(total))
  |> print()
```

```text output
+--------+-------+--------+
| region | total | orders |
+--------+-------+--------+
| east   | 219.8 | 3      |
| north  | 172.5 | 4      |
| south  | 30.0  | 2      |
+--------+-------+--------+
```

อ่านจากบนลงล่างได้เลย:

1. `type Sale = {...}` ประกาศชนิดของแถว `price: float?` แปลว่า column นี้ว่างได้ (ในไฟล์มีแถวที่ไม่มีราคา)
2. `read_csv<Sale>(...)` ได้ตารางที่มี column ตาม `Sale` path ของไฟล์นับจากตำแหน่งของไฟล์โปรแกรม
   ไม่ใช่จากที่ที่สั่งรัน
3. `a |> f(b)` คือ `f(a, b)` ตารางทางซ้ายถูกส่งเป็น argument แรกของฟังก์ชันทางขวา
4. `where` เลือกแถว, `derive` เพิ่ม column, `group` + `agg` สรุปเป็นกลุ่ม, `sort` เรียง
5. `price ?? 0.0` คือ "ใช้ `price` ถ้ามีค่า ถ้าเป็น null ใช้ `0.0`"

ตารางใน biggo เป็น *คำสั่งที่ยังไม่ได้รัน* (lazy) ทั้ง pipeline ถูกรวมเป็นแผนเดียว ปรับให้เหมาะ
แล้วค่อยรันเมื่อถึง `print` — อ่านเฉพาะ column ที่ใช้ กรองแถวตั้งแต่ตอนอ่านไฟล์ และใช้ทุก core ของเครื่อง

## ผิดแล้วรู้ก่อนรัน

ลองพิมพ์ชื่อ column ผิด:

```biggo error
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")
print(sales |> where(quantity > 0))
```

```text output
error: undefined name `quantity`; the table has columns date, region, product, qty, price
 --> example.bgo:3:22
  |
3 | print(sales |> where(quantity > 0))
  |                      ^^^^^^^^
```

ข้อผิดพลาดนี้มาจากตัวตรวจ type ยังไม่มีการเปิดไฟล์ข้อมูลเลย ถ้าอยากตรวจอย่างเดียวโดยไม่รัน
ใช้ `biggo check hello.bgo`

## ลองทีละบรรทัดด้วย REPL

`biggo repl` เปิดโหมดโต้ตอบ พิมพ์ expression แล้วเห็นค่าทันที ตัวแปรและฟังก์ชันที่ประกาศไว้อยู่ต่อไปจนปิด:

```text
$ biggo repl
biggo 0.1.0 (Ctrl-D to exit)
>> let x = 2
>> x * 21
42
>> fn double(n: int) -> int { n * 2 }
>> [1, 2, 3] |> map(double)
[2, 4, 6]
>> [x, x + 1] |>
..   print()
[2, 3]
```

ถ้าบรรทัดยังไม่จบ (เช่นวงเล็บยังไม่ปิด หรือจบด้วย `|>`) REPL จะรอบรรทัดถัดไปเอง

## ดูว่า engine จะทำอะไร

`biggo explain hello.bgo` พิมพ์แผนของทุก query แทนการรัน ทั้งแผนที่เขียนและแผนหลังปรับ:

```biggo explain
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
read_csv<Sale>("data/sales.csv")
  |> where(qty > 0)
  |> group(region)
  |> agg(units = sum(qty))
  |> print()
```

```text output
plan:
  Aggregate: by region; units = sum(qty)
    Filter: qty > 0
      Scan csv "data/sales.csv": date, region, product, qty, price
optimized plan:
  Aggregate: by region; units = sum(qty)
    Scan csv "data/sales.csv": region, qty where qty > 0
```

แผนหลังปรับอ่านแค่ 2 จาก 5 column และกรอง `qty > 0` ไปในขั้นอ่านไฟล์เลย

## ไปต่อ

| อยากรู้เรื่อง | อ่าน |
| --- | --- |
| ไวยากรณ์ ตัวแปร ฟังก์ชัน `if` `match` lambda | [ตัวภาษา](02-language.md) |
| ชนิดข้อมูลทั้งหมดและกฎการแปลงชนิด | [ระบบ type](03-types.md) |
| `where` `group` `join` `window` `pivot` ฯลฯ | [การทำงานกับตาราง](04-tables.md) |
| CSV, Parquet, JSON, SQLite | [แหล่งข้อมูล](05-data-sources.md) |
| ฟังก์ชันสำเร็จรูปทุกตัว | [built-in reference](06-builtins.md) |
| `run` `check` `fmt` `test` `build` `lsp` และ editor | [เครื่องมือ](07-tools.md) |
| compiler ทำงานอย่างไร | [สถาปัตยกรรม](08-architecture.md) |
| ผล benchmark และวิธีวัด | [ประสิทธิภาพ](09-performance.md) |
| ไวยากรณ์แบบเป็นทางการ | [grammar](10-grammar.md) |
