# แหล่งข้อมูล: CSV, Parquet, JSON, SQLite

biggo อ่านและเขียนข้อมูลได้ 4 รูปแบบ ทุกแบบใช้หลักเดียวกัน:

- **อ่าน**: `read_xxx<ชนิดของแถว>(path)` ได้ตาราง โปรแกรมบอกว่าต้องการ column อะไร ชนิดอะไร
  แล้ว engine ตรวจกับไฟล์ตอนรัน
- **เขียน**: `write_xxx(ตาราง, path)` รัน query แล้วเขียนผลลัพธ์ทั้งหมด เขียนทับถ้ามีไฟล์อยู่แล้ว
  และสร้างโฟลเดอร์ให้ถ้ายังไม่มี
- **path** นับจากโฟลเดอร์ของไฟล์โปรแกรม ไม่ใช่จากที่ที่สั่งรัน โปรแกรมจึงย้ายไปรันจากที่ไหนก็ได้
  พร้อมข้อมูลของมัน
- การอ่านเป็นแบบ lazy: `read_xxx` ยังไม่เปิดไฟล์ ไฟล์ถูกอ่านเมื่อ query รัน และอ่านเฉพาะ column
  ที่ query ใช้

| รูปแบบ | อ่าน | เขียน | อ่านขนานหลาย core | ข้าม column ที่ไม่ใช้ |
| --- | --- | --- | --- | --- |
| CSV | `read_csv` | `write_csv` | ✓ | ✓ (ไม่แปลงค่า) |
| Parquet | `read_parquet` | `write_parquet` | ✓ | ✓ (ไม่อ่านจากดิสก์เลย) |
| JSON (หนึ่ง object ต่อบรรทัด) | `read_json` | `write_json` | ✓ | ✓ |
| SQLite | `read_sql` | `write_sql` | | (เขียนใน query เอง) |

## ชนิดของแถว

type argument ใน `<...>` บอก column ที่โปรแกรมต้องการ:

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }

// ประกาศเท่าที่ใช้ก็ได้ ลำดับไม่ต้องตรงกับไฟล์ จับคู่ด้วยชื่อ column
let slim = read_csv<{ qty: int, product: string }>("data/sales.csv")
print(slim |> take(2))
print(read_csv<Sale>("data/sales.csv") |> count())
```

```text output
+-----+---------+
| qty | product |
+-----+---------+
| 10  | widget  |
| 3   | gadget  |
+-----+---------+
10
```

- ไฟล์มี column มากกว่าที่ประกาศได้ ส่วนเกินถูกข้าม
- column ที่ประกาศแต่ไฟล์ไม่มีเป็น error ตอนรัน พร้อมรายชื่อ column ที่ไฟล์มี
- column ที่ประกาศว่าไม่เป็น null (`qty: int`) แต่ไฟล์มีค่าว่าง เป็น error ที่บอกให้ประกาศเป็น `int?`
- ค่าที่แปลงเป็นชนิดที่ประกาศไม่ได้เป็น error ที่บอกบรรทัดและ column

```biggo error
print(read_csv<{ product: string, qty: float, price: float }>("data/sales.csv"))
```

```text output
error: column `price` of data/sales.csv has missing values, but is declared `float`; declare it `float?`
 --> example.bgo:1:1
  |
1 | print(read_csv<{ product: string, qty: float, price: float }>("data/sales.csv"))
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

```biggo error
print(read_csv<{ product: int }>("data/sales.csv"))
```

```text output
error: data/sales.csv, line 2: cannot read 'widget' as an int for column `product`
 --> example.bgo:1:1
  |
1 | print(read_csv<{ product: int }>("data/sales.csv"))
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

error พวกนี้เกิดตอนรัน เพราะ compiler ไม่เปิดไฟล์ข้อมูล: โปรแกรมเดียวกันจึงตรวจผ่านได้แม้ยังไม่มีไฟล์
และใช้กับไฟล์ที่เปลี่ยนไปทุกวันได้

## รูปของค่าในไฟล์

ค่าแต่ละชนิดมีรูปเดียวกันในทุกรูปแบบไฟล์ ไฟล์ที่ biggo เขียนจึงอ่านกลับได้เสมอ และตารางเดียวกัน
เขียนเป็น CSV, JSON, Parquet หรือ SQLite แล้วอ่านกลับได้ค่าเท่าเดิมทุกค่า

| type | CSV | JSON | SQLite | Parquet |
| --- | --- | --- | --- | --- |
| `int` | `42` | `42` | `INTEGER` | `INT64` |
| `float` | `2.5` | `2.5` | `REAL` | `DOUBLE` |
| `bool` | `true` / `false` | `true` / `false` | `INTEGER` 1 / 0 | `BOOLEAN` |
| `string` | ข้อความ | `"ข้อความ"` | `TEXT` | `STRING` |
| `date` | `2026-01-31` | `"2026-01-31"` | `TEXT` | `DATE` |
| `datetime` | `2026-01-31T18:30:00` | `"2026-01-31T18:30:00"` | `TEXT` | `TIMESTAMP` (ไมโครวินาที) |
| `decimal` | `19.99` | `"19.99"` | `TEXT` | `DECIMAL(38, 6)` |
| `duration` | จำนวนวินาที `5400.0` | จำนวนวินาที `5400.0` | `REAL` (วินาที) | duration ของ Arrow |
| null | ช่องว่าง | ไม่มี field หรือ `null` | `NULL` | null |

ตอนอ่าน ตัวอ่านยอมรับรูปที่กว้างกว่าที่เขียน:

- `datetime`: ใช้ช่องว่างแทน `T` ได้, มีเศษวินาทีได้ถึง 6 หลัก, วันที่อย่างเดียวคือเวลา 00:00
  ถ้ามี time zone ต่อท้าย (`Z`, `+07:00`) ค่าถูกแปลงเป็น UTC แล้วทิ้ง zone **ต้องมีวินาที**
  (`18:30:00` ไม่ใช่ `18:30`)
- `decimal`: ใน JSON เป็นตัวเลขหรือ string ก็ได้ ทศนิยมเกิน 6 ตำแหน่งถูกปัด
- `bool` ใน CSV: `true` / `false` ตัวพิมพ์ใดก็ได้ (`1`/`0` ไม่ได้)
- `duration`: ตัวเลขทุกชนิดถูกอ่านเป็นจำนวนวินาที

## CSV

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")

let summary = sales
  |> group(region)
  |> agg(units = sum(qty), best = max(price), latest = max(date))
  |> sort(region)
write_csv(summary, "out/summary.csv")

print(read_csv<{ region: string, units: int, best: float?, latest: date }>("out/summary.csv"))
```

```text output
+--------+-------+-------+------------+
| region | units | best  | latest     |
+--------+-------+-------+------------+
| east   | 7     | 99.9  | 2026-03-09 |
| north  | 21    | 100.0 | 2026-03-15 |
| south  | 17    | 2.5   | 2026-03-01 |
+--------+-------+-------+------------+
```

ไฟล์ที่ได้:

```text
region,units,best,latest
east,7,99.9,2026-03-09
north,21,100.0,2026-03-15
south,17,2.5,2026-03-01
```

ข้อกำหนดของ CSV ที่อ่านได้:

- บรรทัดแรกเป็นชื่อ column (header) เสมอ
- คั่นด้วย `,` เท่านั้น
- ค่าที่มี `,` การขึ้นบรรทัดใหม่ หรือ `"` ต้องอยู่ใน `"..."` และ `"` ข้างในเขียนซ้ำเป็น `""`
- เข้ารหัสแบบ UTF-8; BOM ต้นไฟล์ถูกข้าม
- ช่องว่างเปล่าคือ null ยกเว้น column ชนิด `string` ที่ไม่เป็น nullable ซึ่งได้ string ว่าง `""`
- ทุกบรรทัดต้องมีจำนวนช่องเท่ากับ header

**ความเร็ว:** ไฟล์ถูก map เข้าหน่วยความจำแล้วตัดเป็นชิ้นละ 2 MiB ที่ขอบบรรทัด (นับเครื่องหมาย `"`
เพื่อไม่ตัดกลางค่าที่มีการขึ้นบรรทัดใหม่) แต่ละชิ้นถูกแปลงบน core ของตัวเอง column ที่ query ไม่ใช้
ไม่ถูกแปลงเป็นค่า และตัวกรองของ `where` ถูกใช้ตั้งแต่ในชิ้น

## Parquet

Parquet เป็นรูปแบบแบบ column ที่บีบอัดและมี type ในตัว เร็วกว่า CSV มาก
(ใน [benchmark](09-performance.md) อ่านเร็วกว่าราว 3–4 เท่า) เพราะไม่ต้องแปลงข้อความเป็นค่า
และข้าม column ที่ไม่ใช้ได้โดยไม่อ่านจากดิสก์

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
write_parquet(read_csv<Sale>("data/sales.csv"), "out/sales.parquet")

read_parquet<{ region: string, qty: int }>("out/sales.parquet")
  |> where(qty > 5)
  |> print()
```

```text output
+--------+-----+
| region | qty |
+--------+-----+
| north  | 10  |
| north  | 7   |
| south  | 12  |
+--------+-----+
```

- column จับคู่ด้วยชื่อ ชนิดในไฟล์ต่างจากที่ประกาศได้ถ้าแปลงได้ (เช่นไฟล์เป็น `INT32` ประกาศ `int`)
- เขียนด้วยการบีบอัด Snappy แบ่งเป็น row group ละ 131,072 แถว ซึ่งเป็นหน่วยที่อ่านขนานกันได้
- ไฟล์ที่เขียนจากเครื่องมืออื่น (pandas, Polars, DuckDB, Spark) อ่านได้ และกลับกัน

งานที่อ่านไฟล์ CSV เดิมซ้ำหลายรอบ ควรแปลงเป็น Parquet ครั้งเดียวแล้วอ่านจาก Parquet

## JSON

`read_json` อ่านไฟล์แบบ *JSON Lines* (NDJSON): หนึ่ง object ต่อหนึ่งบรรทัด
ตัวอย่างไฟล์ [`data/events.json`](data/events.json):

```text
{"id": 1, "kind": "click", "at": "2026-01-05T09:00:00", "cost": "0.25", "ok": true}
{"id": 2, "kind": "view", "at": "2026-01-05T09:01:30", "cost": null, "ok": true}
{"id": 3, "kind": "click", "at": "2026-01-06T17:45:00", "cost": 1.5, "ok": false, "extra": "ignored"}

{"id": 4, "kind": "click", "at": "2026-01-07T08:00:00", "ok": true}
```

```biggo
type Event = { id: int, kind: string, at: datetime, cost: decimal?, ok: bool }
let events = read_json<Event>("data/events.json")
print(events)

write_json(events |> where(ok) |> select(id, day = to_date(at), cost), "out/ok.json")
print(read_json<{ id: int, day: date, cost: decimal? }>("out/ok.json"))
```

```text output
+----+-------+---------------------+------+-------+
| id | kind  | at                  | cost | ok    |
+----+-------+---------------------+------+-------+
| 1  | click | 2026-01-05T09:00:00 | 0.25 | true  |
| 2  | view  | 2026-01-05T09:01:30 | null | true  |
| 3  | click | 2026-01-06T17:45:00 | 1.5  | false |
| 4  | click | 2026-01-07T08:00:00 | null | true  |
+----+-------+---------------------+------+-------+
+----+------------+------+
| id | day        | cost |
+----+------------+------+
| 1  | 2026-01-05 | 0.25 |
| 2  | 2026-01-05 | null |
| 4  | 2026-01-07 | null |
+----+------------+------+
```

- field ที่ object ไม่มี หรือมีค่า `null` อ่านเป็น null
- field ที่ไม่ได้ประกาศถูกข้าม บรรทัดว่างถูกข้าม
- ค่าต้องเป็นค่าเดี่ยว: field ที่เป็น object หรือ array ซ้อนอ่านเป็น column ไม่ได้
- ไฟล์ที่เป็น array ใหญ่ก้อนเดียว (`[{...}, {...}]`) ไม่ใช่ JSON Lines และอ่านไม่ได้
  แปลงก่อนด้วยเครื่องมืออย่าง `jq -c '.[]'`
- `write_json` เขียนหนึ่ง object ต่อบรรทัด field ที่เป็น null ถูกละไว้

## SQLite

`read_sql<T>(path, query)` รัน query บนไฟล์ฐานข้อมูล SQLite แล้วได้ผลเป็นตาราง
`write_sql(table, path, name)` เขียนตารางลงฐานข้อมูลภายใต้ชื่อ `name`

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")

// เขียนสองตารางลงฐานข้อมูลเดียว (สร้างไฟล์ให้ถ้ายังไม่มี)
write_sql(sales, "out/shop.db", "sales")
write_sql(sales |> group(product) |> agg(units = sum(qty)), "out/shop.db", "product_units")

// query เป็น SQL ของ SQLite เต็มรูปแบบ ชื่อ column ของผลลัพธ์ต้องตรงกับที่ประกาศ
type Row = { region: string, day: date, revenue: float }
read_sql<Row>(
  "out/shop.db",
  "select region, date as day, qty * price as revenue from sales where price is not null order by revenue desc limit 3",
)
  |> print()

// ผลของ query ใช้ต่อใน pipeline ได้เหมือนตารางอื่น
read_sql<{ product: string, units: int }>("out/shop.db", "select * from product_units")
  |> where(units > 5)
  |> sort(desc(units))
  |> print()
```

```text output
+--------+------------+---------+
| region | day        | revenue |
+--------+------------+---------+
| east   | 2026-02-20 | 199.8   |
| north  | 2026-03-15 | 100.0   |
| north  | 2026-01-17 | 30.0    |
+--------+------------+---------+
+---------+-------+
| product | units |
+---------+-------+
| widget  | 33    |
| gadget  | 9     |
+---------+-------+
```

- ใช้ได้กับไฟล์ SQLite เท่านั้น (ตัว SQLite ถูกฝังมาใน `biggo` ไม่ต้องติดตั้งเพิ่ม)
  ยังไม่รองรับฐานข้อมูลแบบ server อย่าง PostgreSQL หรือ MySQL
- ฐานข้อมูลถูกเปิดแบบอ่านอย่างเดียวตอน `read_sql`
- column ของผลลัพธ์จับคู่ด้วยชื่อ ใช้ `as` ใน SQL ตั้งชื่อให้ตรงกับ type ที่ประกาศ
- ค่าถูกแปลงตามชนิดที่ประกาศ: `TEXT` ถูกแปลงเป็น `date` `datetime` `decimal` หรือตัวเลขได้,
  `INTEGER` เป็น `bool` (0 คือ `false`) หรือ `float` ได้
- `write_sql` **แทนที่** ตารางชื่อนั้นทั้งตาราง (ตารางอื่นในฐานข้อมูลไม่ถูกแตะ)
  การเขียนอยู่ใน transaction เดียว ถ้าล้มเหลวกลางทางตารางเดิมยังอยู่
- `where` ที่ต่อหลัง `read_sql` ทำงานใน biggo ไม่ได้ถูกส่งเข้าไปใน SQL: ถ้าตารางใหญ่ให้กรองใน query
- การอ่านจาก SQLite ทำงานบน thread เดียว

## error ที่พบบ่อย

| ข้อความ | สาเหตุ |
| --- | --- |
| `cannot open x.csv: No such file or directory` | path ผิด — นับจากโฟลเดอร์ของไฟล์โปรแกรม |
| `x.csv has no column `c`; its columns are a, b` | ชื่อ column ไม่ตรงกับ header (ตัวพิมพ์เล็ก/ใหญ่ถือว่าต่างกัน) |
| `column `c` of x.csv has missing values, but is declared `int`; declare it `int?`` | มีค่าว่างใน column ที่ไม่ได้ประกาศเป็น nullable |
| `x.csv, line 12: cannot read 'abc' as an int for column `c`` | ค่าในบรรทัดนั้นไม่ใช่ชนิดที่ประกาศ |
| `x.csv, line 12: the row has 3 fields, but the header has 5` | จำนวนช่องไม่เท่า header มักเกิดจาก `,` หรือ `"` ในค่าที่ไม่ได้ครอบด้วย `"..."` |
| `x.json: cannot read "abc" as an int for field `c`` | ค่าของ field ไม่ใช่ชนิดที่ประกาศ |
| `x.json: every line must be one JSON object` | ไฟล์ไม่ใช่ JSON Lines |
| `x.db: no such table: t` | ข้อความจาก SQLite โดยตรง |

error จากข้อมูลชี้ไปที่บรรทัดของโปรแกรมที่ *รัน* query (เช่น `print`) ไม่ใช่บรรทัดที่เขียน
`read_csv` เพราะไฟล์ถูกอ่านเมื่อ query รัน
