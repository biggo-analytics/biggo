# การทำงานกับตาราง

ตารางคือชนิดข้อมูลหลักของ biggo หน้านี้ครอบคลุมทุก operation ของตาราง
ตัวอย่างทั้งหน้าใช้ตาราง `sales` นี้ (ไฟล์ [`data/sales.csv`](data/sales.csv)):

```biggo prelude
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("data/sales.csv")
```

```biggo
print(sales)
```

```text output
+------------+--------+---------+-----+-------+
| date       | region | product | qty | price |
+------------+--------+---------+-----+-------+
| 2026-01-05 | north  | widget  | 10  | 2.5   |
| 2026-01-17 | north  | gadget  | 3   | 10.0  |
| 2026-01-20 | south  | widget  | 0   | 2.5   |
| 2026-02-02 | south  | gadget  | 5   | null  |
| 2026-02-14 | north  | widget  | 7   | 2.5   |
| 2026-02-20 | east   | gizmo   | 2   | 99.9  |
| 2025-12-30 | east   | widget  | 4   | 2.5   |
| 2026-03-01 | south  | widget  | 12  | 2.5   |
| 2026-03-09 | east   | gadget  | 1   | 10.0  |
| 2026-03-15 | north  | gizmo   | 1   | 100.0 |
+------------+--------+---------+-----+-------+
```

## ตารางคือ query ที่ยังไม่ได้รัน

`read_csv` ไม่ได้อ่านไฟล์ และ `where` ไม่ได้กรองอะไรทันที แต่ละ operation แค่ *ต่อแผน* ให้ยาวขึ้น
ตัวแปรที่เก็บตารางจึงเก็บแผน ไม่ใช่ข้อมูล แผนถูกรันเมื่อโปรแกรมต้องการผลลัพธ์จริง คือเมื่อเรียก

| ฟังก์ชัน | สิ่งที่ทำ |
| --- | --- |
| `print(t)` | รันแล้วพิมพ์ 50 แถวแรก |
| `write_csv` `write_parquet` `write_json` `write_sql` | รันแล้วเขียนผลลัพธ์ทั้งหมด |
| `count(t)` | รันแล้วให้จำนวนแถว (`int`) |
| `collect(t)` | รันแล้วเก็บผลลัพธ์ไว้ในหน่วยความจำ ได้ตารางใหม่ |
| `to_rows(t)` | รันแล้วให้ `list` ของ record |
| `describe(t)` `histogram(t, ...)` `linreg(t, ...)` | รันเพื่อคำนวณสถิติ |
| `assert_eq(t1, t2)` | รันทั้งสองตารางแล้วเทียบ |

ก่อนรัน แผนทั้งก้อนถูกปรับ (optimize): กรองให้เร็วที่สุด อ่านเฉพาะ column ที่ใช้
หยุดอ่านเมื่อได้แถวครบ ดูได้ด้วย `explain(t)` หรือคำสั่ง `biggo explain`

```biggo explain
sales
  |> derive(revenue = qty * (price ?? 0.0))
  |> where(region == "north")
  |> select(product, revenue)
  |> take(2)
  |> print()
```

```text output
plan:
  Limit: take 2
    Project: product, revenue
      Filter: region == "north"
        Project: date, region, product, qty, price, revenue = float(qty) * (price ?? 0.0)
          Scan csv "data/sales.csv": date, region, product, qty, price
optimized plan:
  Project: product, revenue = float(qty) * (price ?? 0.0)
    Limit: take 2
      Scan csv "data/sales.csv": product, qty, price where region == "north" limit 2
```

ผลที่ตามมาคือ **ตารางที่ใช้สองครั้งถูกคำนวณสองครั้ง** ถ้าตารางกลางทางคำนวณแพงและใช้หลายที่
ให้ `collect` ไว้ก่อน:

```biggo
let large = collect(sales |> where(qty > 5))      // อ่านไฟล์ครั้งเดียวตรงนี้
print(count(large), large |> agg(units = sum(qty)))
```

```text output
3
+-------+
| units |
+-------+
| 29    |
+-------+
```

## expression ของ column

ภายใน argument ของ operation อย่าง `where` `derive` `agg` ชื่อ column ใช้ได้เหมือนตัวแปร
expression แบบนี้ถูกคำนวณ *ทั้ง column ในคราวเดียว* โดย engine ไม่ใช่ทีละแถวโดยตัวแปลภาษา
สิ่งที่ใช้ได้ใน expression ของ column:

- ชื่อ column, literal, และค่าจากโปรแกรม (ตัวแปร ผลของฟังก์ชัน)
- ตัวดำเนินการทั้งหมด: คณิตศาสตร์ เปรียบเทียบ `and` `or` `not` `??`
- `if ... else ...` (ต้องมี `else`) และ `match`
- built-in ที่ทำงานกับค่าเดี่ยว: `round` `upper` `year` `to_int` `is_null` ฯลฯ
  ([รายการเต็ม](06-builtins.md#ฟังก์ชันของค่าเดี่ยว))

```biggo
let min_qty = 4                                   // ค่าจากโปรแกรมใช้ใน expression ได้
fn tax_rate() -> float { 0.07 }

sales
  |> where(qty >= min_qty and region != "east")
  |> derive(
    revenue = qty * (price ?? 0.0),
    label = upper(region) + "-" + product,
    size = match qty { 4 | 5 => "small", _ => "large" },
  )
  |> derive(taxed = round(revenue * (1 + tax_rate()), 2))   // ใช้ column ที่เพิ่งสร้างได้
  |> select(date, label, size, revenue, taxed)
  |> print()
```

```text output
+------------+--------------+-------+---------+-------+
| date       | label        | size  | revenue | taxed |
+------------+--------------+-------+---------+-------+
| 2026-01-05 | NORTH-widget | large | 25.0    | 26.75 |
| 2026-02-02 | SOUTH-gadget | small | 0.0     | 0.0   |
| 2026-02-14 | NORTH-widget | large | 17.5    | 18.73 |
| 2026-03-01 | SOUTH-widget | large | 30.0    | 32.1  |
+------------+--------------+-------+---------+-------+
```

ส่วนของ expression ที่ไม่แตะ column เลย (อย่าง `1 + tax_rate()`) ถูกคำนวณ **ครั้งเดียว**
ตอนสร้างแผน แล้วฝังค่าลงไป ไม่ได้ถูกเรียกซ้ำทุกแถว

สิ่งที่ทำไม่ได้: ส่ง column เข้าฟังก์ชันที่เขียนเอง

```biggo error
fn double(n: int) -> int { n * 2 }
print(sales |> derive(twice = double(qty)))
```

```text output
error: a user-defined function cannot take a column; only operators and built-in functions work on columns
 --> example.bgo:2:38
  |
2 | print(sales |> derive(twice = double(qty)))
  |                                      ^^^
```

เพราะฟังก์ชันที่เขียนเองทำงานทีละค่าบนตัวแปลภาษา ซึ่งช้ากว่า engine หลายร้อยเท่า ภาษาจึงไม่ยอมให้
เผลอเขียน pipeline ที่ช้าโดยไม่รู้ตัว ถ้าต้องการใช้ซ้ำ ให้เขียนฟังก์ชันที่ *รับและคืนตาราง* แทน
(ดู [ฟังก์ชันที่รับและคืนตาราง](#ฟังก์ชันที่รับและคืนตาราง))

กฎอื่นที่ควรรู้:

- ตัวแปรกับ column ชื่อชนกันเป็น compile error (`is both a column of this table and a variable`)
  ให้เปลี่ยนชื่อตัวแปร
- null ทำงานแบบเดียวกับนอกตาราง: `price > 50` ของแถวที่ `price` เป็น null ได้ null
- column ที่ชื่อมีช่องว่างหรือตรงกับ keyword ครอบด้วย backtick: `` `order date` ``
- expression ที่ผิดพลาดได้ตอนรัน (เช่น `%` ด้วยศูนย์) ถูกคำนวณเฉพาะแถวที่ต้องใช้จริง:
  `if n != 0 { 10 % n } else { 0 }` ปลอดภัยเสมอ

## เลือกแถว

```biggo
// where: เก็บแถวที่เงื่อนไขเป็น true (แถวที่เงื่อนไขเป็น null ถูกทิ้ง)
print(sales |> where(price > 50) |> select(product, price))
print(sales |> where(is_null(price)))

// take / skip: n แถวแรก / ข้าม n แถวแรก
print(sales |> sort(desc(qty)) |> skip(1) |> take(2) |> select(product, qty))

// distinct: ตัดแถวซ้ำ ทั้งแถว หรือเฉพาะ column ที่ระบุ
print(sales |> distinct(region))
print(sales |> select(region, product) |> distinct() |> count())
```

```text output
+---------+-------+
| product | price |
+---------+-------+
| gizmo   | 99.9  |
| gizmo   | 100.0 |
+---------+-------+
+------------+--------+---------+-----+-------+
| date       | region | product | qty | price |
+------------+--------+---------+-----+-------+
| 2026-02-02 | south  | gadget  | 5   | null  |
+------------+--------+---------+-----+-------+
+---------+-----+
| product | qty |
+---------+-----+
| widget  | 10  |
| widget  | 7   |
+---------+-----+
+--------+
| region |
+--------+
| north  |
| south  |
| east   |
+--------+
8
```

| operation | ผลลัพธ์ |
| --- | --- |
| `where(cond)` | แถวที่ `cond` เป็น `true`; `cond` เป็น `bool` หรือ `bool?` |
| `take(n)` | `n` แถวแรก |
| `skip(n)` | ทุกแถวยกเว้น `n` แถวแรก |
| `distinct()` | แถวที่ไม่ซ้ำกัน เรียงตามที่พบครั้งแรก |
| `distinct(a, b)` | ค่าที่ไม่ซ้ำของ column `a`, `b` (ผลลัพธ์มีเฉพาะ column ที่ระบุ) |

`n` ของ `take`/`skip` เป็น expression ของโปรแกรมได้ (`take(limit * 2)`) แต่ต้องไม่ติดลบ

## เลือกและสร้าง column

```biggo
print(sales |> select(product, qty) |> take(2))
// select สร้าง column ใหม่ได้ด้วย name = expression
print(sales |> select(product, year = year(date), total = qty * (price ?? 0.0)) |> take(2))
print(sales |> drop(date, price) |> rename(quantity = qty, area = region) |> take(2))
// derive เพิ่ม column หรือแทนที่ column ชื่อเดิม
print(sales |> derive(qty = qty * 10, big = qty >= 5) |> take(2))
```

```text output
+---------+-----+
| product | qty |
+---------+-----+
| widget  | 10  |
| gadget  | 3   |
+---------+-----+
+---------+------+-------+
| product | year | total |
+---------+------+-------+
| widget  | 2026 | 25.0  |
| gadget  | 2026 | 30.0  |
+---------+------+-------+
+-------+---------+----------+
| area  | product | quantity |
+-------+---------+----------+
| north | widget  | 10       |
| north | gadget  | 3        |
+-------+---------+----------+
+------------+--------+---------+-----+-------+------+
| date       | region | product | qty | price | big  |
+------------+--------+---------+-----+-------+------+
| 2026-01-05 | north  | widget  | 100 | 2.5   | true |
| 2026-01-17 | north  | gadget  | 30  | 10.0  | true |
+------------+--------+---------+-----+-------+------+
```

| operation | ผลลัพธ์ |
| --- | --- |
| `select(a, b, c = expr)` | เฉพาะ column ที่ระบุ ตามลำดับที่เขียน |
| `drop(a, b)` | ทุก column ยกเว้นที่ระบุ |
| `rename(new = old)` | เปลี่ยนชื่อ ตำแหน่งเดิม |
| `derive(c = expr)` | ทุก column เดิม บวก column ใหม่ต่อท้าย; ชื่อซ้ำกับของเดิมคือแทนที่ |

ใน `derive` เดียวกัน column หลังใช้ column ที่ประกาศก่อนหน้าได้ ส่วนในตัวอย่างข้างบน
`big = qty >= 5` เห็น `qty` ที่ถูกคูณ 10 แล้ว

## เรียงลำดับ

```biggo
print(sales |> sort(region, desc(qty)) |> select(region, product, qty) |> take(5))
print(sales |> sort(desc(price), product) |> select(product, price) |> skip(7))
```

```text output
+--------+---------+-----+
| region | product | qty |
+--------+---------+-----+
| east   | widget  | 4   |
| east   | gizmo   | 2   |
| east   | gadget  | 1   |
| north  | widget  | 10  |
| north  | widget  | 7   |
+--------+---------+-----+
+---------+-------+
| product | price |
+---------+-------+
| widget  | 2.5   |
| widget  | 2.5   |
| gadget  | null  |
+---------+-------+
```

- `sort(a, b)` เรียงตาม `a` ก่อน ถ้าเท่ากันจึงดู `b` ค่าเริ่มต้นคือน้อยไปมาก
- `desc(x)` กลับเป็นมากไปน้อย, `asc(x)` เขียนให้ชัดได้ว่าน้อยไปมาก
- key เป็น expression ได้: `sort(desc(qty * price))`
- **null อยู่ท้ายเสมอ** ทั้งแบบ `asc` และ `desc`
- แถวที่ key เท่ากันคงลำดับเดิม (stable sort)
- `sort` ตามด้วย `take(n)` ถูกรวมเป็น top-n: ไม่ต้องเรียงทั้งตาราง

## สรุปเป็นกลุ่ม: group และ agg

`group(...)` แบ่งแถวเป็นกลุ่มตามค่าของ column แล้ว `agg(...)` ย่อแต่ละกลุ่มเหลือหนึ่งแถว

```biggo
sales
  |> group(region)
  |> agg(
    orders = count(),            // จำนวนแถว
    priced = count(price),       // จำนวนค่าที่ไม่เป็น null
    units = sum(qty),
    avg_qty = mean(qty),
    high = max(price),
    products = count_distinct(product),
  )
  |> sort(region)
  |> print()
```

```text output
+--------+--------+--------+-------+--------------------+-------+----------+
| region | orders | priced | units | avg_qty            | high  | products |
+--------+--------+--------+-------+--------------------+-------+----------+
| east   | 3      | 3      | 7     | 2.3333333333333335 | 99.9  | 3        |
| north  | 4      | 4      | 21    | 5.25               | 100.0 | 3        |
| south  | 3      | 2      | 17    | 5.666666666666667  | 2.5   | 2        |
+--------+--------+--------+-------+--------------------+-------+----------+
```

aggregate ที่มี:

| ฟังก์ชัน | ผลลัพธ์ | รับ |
| --- | --- | --- |
| `count()` | จำนวนแถวของกลุ่ม | — |
| `count(x)` | จำนวนค่าที่ไม่เป็น null | ทุกชนิด |
| `count_distinct(x)` | จำนวนค่าที่ต่างกัน (ไม่นับ null) | ทุกชนิด |
| `sum(x)` | ผลรวม | ตัวเลข, `duration` |
| `mean(x)` | ค่าเฉลี่ย (`float`) | ตัวเลข |
| `median(x)` | มัธยฐาน (`float`) | ตัวเลข |
| `stddev(x)` | ส่วนเบี่ยงเบนมาตรฐานของตัวอย่าง (หารด้วย n−1) | ตัวเลข |
| `min(x)` `max(x)` | ค่าน้อยสุด/มากสุด | ทุกชนิดยกเว้น `bool` |
| `first(x)` `last(x)` | ค่าของแถวแรก/สุดท้ายของกลุ่ม | ทุกชนิด |
| `corr(y, x)` `cov(y, x)` | สหสัมพันธ์, ความแปรปรวนร่วม | ตัวเลข |
| `slope(y, x)` `intercept(y, x)` | เส้นถดถอยเชิงเส้น | ตัวเลข |

ทุก aggregate ข้าม null (ยกเว้น `count()` ที่นับแถว) เมื่อไม่มีค่าให้สรุปเลย `count` ได้ `0`
ตัวอื่นได้ null — ยกเว้น `sum` ของ column ที่ไม่ใช่ nullable บนตารางว่าง ซึ่งได้ `0`

argument ของ aggregate เป็น expression ได้ และผลของ aggregate นำมาคำนวณต่อได้:

```biggo
sales
  |> group(month = month(date), large = qty >= 5)      // key ของกลุ่มคำนวณได้
  |> agg(
    per_order = sum(qty) / count(),
    revenue = round(sum(qty * (price ?? 0.0)), 1),
  )
  |> sort(month, large)
  |> print()
```

```text output
+-------+-------+-----------+---------+
| month | large | per_order | revenue |
+-------+-------+-----------+---------+
| 1     | false | 1.5       | 30.0    |
| 1     | true  | 10.0      | 25.0    |
| 2     | false | 2.0       | 199.8   |
| 2     | true  | 6.0       | 17.5    |
| 3     | false | 1.0       | 110.0   |
| 3     | true  | 12.0      | 30.0    |
| 12    | false | 4.0       | 10.0    |
+-------+-------+-----------+---------+
```

`agg` โดยไม่มี `group` สรุปทั้งตารางเป็นแถวเดียว (ได้หนึ่งแถวเสมอ แม้ตารางว่าง):

```biggo
print(sales |> agg(rows = count(), total = sum(qty), best = max(price)))
print(sales |> where(qty > 1000) |> agg(rows = count(), total = sum(qty), best = max(price)))
```

```text output
+------+-------+-------+
| rows | total | best  |
+------+-------+-------+
| 10   | 45    | 100.0 |
+------+-------+-------+
+------+-------+------+
| rows | total | best |
+------+-------+------+
| 0    | 0     | null |
+------+-------+------+
```

กฎของ `agg`:

- ทุก argument ต้องเป็น `name = expression` และ expression ต้องเป็น aggregate
  หรือคำนวณจาก aggregate กับ column ของกลุ่ม
- column ที่ไม่ใช่ key ของกลุ่มต้องอยู่ใน aggregate: `agg(x = qty)` เป็น error
- ผลลัพธ์มี column ของกลุ่มตามด้วย column ที่ประกาศใน `agg`
- **ลำดับของกลุ่มในผลลัพธ์คือลำดับที่แต่ละกลุ่มปรากฏครั้งแรกในข้อมูล** ถ้าต้องการลำดับอื่นให้ `sort`
- ผลของ `group` (ที่ยังไม่ `agg`) ใช้ได้กับ `agg` และ `pivot` เท่านั้น แต่เก็บในตัวแปรแล้ว `agg`
  หลายแบบได้

## join

`join` จับคู่แถวของสองตารางที่ key เท่ากัน

```biggo
type Order = { order_id: int, customer_id: int?, amount: float }
type Customer = { customer_id: int, name: string, city: string }
let orders = read_csv<Order>("data/orders.csv")
let customers = read_csv<Customer>("data/customers.csv")
print(orders)
print(customers)

// inner join (ค่าเริ่มต้น): เฉพาะแถวที่จับคู่ได้
print(orders |> join(customers, on = customer_id) |> sort(order_id))
// left join: ทุกแถวของตารางซ้าย ฝั่งขวาเป็น null ถ้าไม่มีคู่
print(orders |> join(customers, on = customer_id, how = "left") |> sort(order_id))
// semi / anti: แถวของตารางซ้ายที่ มี / ไม่มี คู่ในตารางขวา
print(customers |> join(orders, on = customer_id, how = "anti"))
```

```text output
+----------+-------------+--------+
| order_id | customer_id | amount |
+----------+-------------+--------+
| 1        | 10          | 25.0   |
| 2        | 11          | 10.5   |
| 3        | 10          | 4.0    |
| 4        | 13          | 99.0   |
| 5        | null        | 1.0    |
+----------+-------------+--------+
+-------------+------+------------+
| customer_id | name | city       |
+-------------+------+------------+
| 10          | Ann  | Bangkok    |
| 11          | Bob  | Chiang Mai |
| 12          | Cho  | Phuket     |
+-------------+------+------------+
+----------+-------------+--------+------+------------+
| order_id | customer_id | amount | name | city       |
+----------+-------------+--------+------+------------+
| 1        | 10          | 25.0   | Ann  | Bangkok    |
| 2        | 11          | 10.5   | Bob  | Chiang Mai |
| 3        | 10          | 4.0    | Ann  | Bangkok    |
+----------+-------------+--------+------+------------+
+----------+-------------+--------+------+------------+
| order_id | customer_id | amount | name | city       |
+----------+-------------+--------+------+------------+
| 1        | 10          | 25.0   | Ann  | Bangkok    |
| 2        | 11          | 10.5   | Bob  | Chiang Mai |
| 3        | 10          | 4.0    | Ann  | Bangkok    |
| 4        | 13          | 99.0   | null | null       |
| 5        | null        | 1.0    | null | null       |
+----------+-------------+--------+------+------------+
+-------------+------+--------+
| customer_id | name | city   |
+-------------+------+--------+
| 12          | Cho  | Phuket |
+-------------+------+--------+
```

| `how` | แถวในผลลัพธ์ |
| --- | --- |
| `"inner"` (ค่าเริ่มต้น) | คู่ที่ key ตรงกัน |
| `"left"` | ทุกแถวของตารางซ้าย; column ฝั่งขวาเป็น null เมื่อไม่มีคู่ |
| `"right"` | ทุกแถวของตารางขวา; column ฝั่งซ้ายเป็น null เมื่อไม่มีคู่ |
| `"full"` | ทุกแถวของทั้งสองตาราง |
| `"semi"` | แถวของตารางซ้ายที่มีคู่ (มีแต่ column ของตารางซ้าย ไม่ซ้ำแถว) |
| `"anti"` | แถวของตารางซ้ายที่ไม่มีคู่ |

การระบุ key:

- `on = key` เมื่อ column ชื่อเดียวกันทั้งสองตาราง ผลลัพธ์มี column key เดียว
- `on = [a, b]` เมื่อ key มีหลาย column
- `left_on = a, right_on = b` เมื่อชื่อต่างกัน ผลลัพธ์เก็บทั้งสอง column

```biggo
type Order = { order_id: int, customer_id: int?, amount: float }
type Customer = { customer_id: int, name: string, city: string }
let orders = read_csv<Order>("data/orders.csv")
let people = read_csv<Customer>("data/customers.csv") |> rename(id = customer_id)

orders
  |> join(people, left_on = customer_id, right_on = id)
  |> select(order_id, id, name, amount)
  |> sort(order_id)
  |> print()

// ยอดรวมต่อเมือง รวมลูกค้าที่ยังไม่มี order
people
  |> join(orders, left_on = id, right_on = customer_id, how = "left")
  |> group(city)
  |> agg(orders = count(order_id), total = sum(amount))
  |> sort(city)
  |> print()
```

```text output
+----------+----+------+--------+
| order_id | id | name | amount |
+----------+----+------+--------+
| 1        | 10 | Ann  | 25.0   |
| 2        | 11 | Bob  | 10.5   |
| 3        | 10 | Ann  | 4.0    |
+----------+----+------+--------+
+------------+--------+-------+
| city       | orders | total |
+------------+--------+-------+
| Bangkok    | 2      | 29.0  |
| Chiang Mai | 1      | 10.5  |
| Phuket     | 0      | null  |
+------------+--------+-------+
```

กฎของ `join`:

- key ที่เป็น null ไม่จับคู่กับอะไรเลย (รวมถึง null ด้วยกัน)
- key ทั้งสองฝั่งต้องเป็นชนิดเดียวกัน (`int` กับ `float` ต้องแปลงฝั่งหนึ่งก่อน)
- column ที่ไม่ใช่ key ชื่อซ้ำกันสองตารางเป็น compile error: `rename` หรือ `drop` ฝั่งหนึ่งก่อน
- type ของผลลัพธ์สะท้อน `how`: ใน left join column ฝั่งขวากลายเป็น `T?` ทั้งหมด
- engine สร้าง hash table จากตารางขวาแล้วไล่ตารางซ้าย: ถ้าทำได้ **ให้ตารางเล็กอยู่ทางขวา**

## window

`window` คำนวณค่าของแต่ละแถวจากแถวอื่นใน "หน้าต่าง" เดียวกัน โดยไม่ยุบแถว
`by` แบ่งตารางเป็น partition และ `order` กำหนดลำดับภายใน partition

```biggo
type Visit = { day: date, site: string, visits: int? }
let log = read_csv<Visit>("data/visits.csv")

log
  |> window(
    by = site,
    order = day,
    n = row_number(),
    prev = lag(visits),
    running = cumsum(visits),
    avg2 = moving_avg(visits, 2),
    site_total = sum(visits),
    share = round(visits / sum(visits), 2),
  )
  |> print()
```

```text output
+------------+------+--------+---+------+---------+------+------------+-------+
| day        | site | visits | n | prev | running | avg2 | site_total | share |
+------------+------+--------+---+------+---------+------+------------+-------+
| 2026-01-01 | a    | 10     | 1 | null | 10      | 10.0 | 45         | 0.22  |
| 2026-01-02 | a    | 15     | 2 | 10   | 25      | 12.5 | 45         | 0.33  |
| 2026-01-03 | a    | null   | 3 | 15   | 25      | 15.0 | 45         | null  |
| 2026-01-04 | a    | 20     | 4 | null | 45      | 20.0 | 45         | 0.44  |
| 2026-01-01 | b    | 7      | 1 | null | 7       | 7.0  | 17         | 0.41  |
| 2026-01-02 | b    | 7      | 2 | 7    | 14      | 7.0  | 17         | 0.41  |
| 2026-01-03 | b    | 3      | 3 | 7    | 17      | 5.0  | 17         | 0.18  |
+------------+------+--------+---+------+---------+------+------------+-------+
```

| ฟังก์ชัน | ค่าของแต่ละแถว |
| --- | --- |
| `row_number()` | ลำดับที่ของแถวใน partition เริ่มจาก 1 |
| `rank()` | อันดับตาม `order`; แถวที่เท่ากันได้อันดับเดียวกัน แล้วข้ามเลขถัดไป |
| `lag(x)`, `lag(x, n)` | ค่าของ `x` ในแถวก่อนหน้า `n` แถว (ค่าเริ่มต้น 1); null ถ้าไม่มี |
| `lead(x)`, `lead(x, n)` | ค่าของ `x` ในแถวถัดไป `n` แถว |
| `cumsum(x)` | ผลรวมสะสมตั้งแต่ต้น partition ถึงแถวนี้ |
| `moving_avg(x, n)` | ค่าเฉลี่ยของแถวนี้กับ `n − 1` แถวก่อนหน้า |
| `sum` `mean` `min` `max` `count` ฯลฯ | aggregate ของทั้ง partition ซ้ำให้ทุกแถว |

- `by` และ `order` รับ column เดียวหรือ list: `by = [site, region]`, `order = [desc(visits), day]`
- ไม่มี `by` คือทั้งตารางเป็น partition เดียว
- ผลลัพธ์มีทุก column เดิมบวก column ที่ประกาศ **จำนวนและลำดับของแถวเหมือนตารางต้นทาง**:
  `order` กำหนดลำดับที่ใช้คำนวณเท่านั้น ถ้าต้องการให้ผลลัพธ์เรียงให้ `sort` ต่อ
- `n` ของ `lag`/`lead`/`moving_avg` ต้องเขียนเป็นตัวเลขตรง ๆ

รูปแบบที่ใช้บ่อย — top-n ต่อกลุ่ม:

```biggo
sales
  |> window(by = region, order = desc(qty), place = row_number())
  |> where(place <= 2)
  |> select(region, place, product, qty)
  |> sort(region, place)
  |> print()
```

```text output
+--------+-------+---------+-----+
| region | place | product | qty |
+--------+-------+---------+-----+
| east   | 1     | widget  | 4   |
| east   | 2     | gizmo   | 2   |
| north  | 1     | widget  | 10  |
| north  | 2     | widget  | 7   |
| south  | 1     | widget  | 12  |
| south  | 2     | gadget  | 5   |
+--------+-------+---------+-----+
```

## union

`union(a, b, ...)` ต่อแถวของตารางที่มี column เหมือนกัน (ชื่อ ชนิด ลำดับ) ไม่ตัดแถวซ้ำ

```biggo
let north = sales |> where(region == "north") |> select(product, qty)
let east = sales |> where(region == "east") |> select(product, qty)
print(union(north, east))
print(union(north, east) |> distinct(product))
```

```text output
+---------+-----+
| product | qty |
+---------+-----+
| widget  | 10  |
| gadget  | 3   |
| widget  | 7   |
| gizmo   | 1   |
| gizmo   | 2   |
| widget  | 4   |
| gadget  | 1   |
+---------+-----+
+---------+
| product |
+---------+
| widget  |
| gadget  |
| gizmo   |
+---------+
```

## แปลงรูปร่าง: pivot, unpivot, explode

### pivot: แถว → column

`pivot(column, [ค่า...], aggregate)` สร้าง column ใหม่หนึ่ง column ต่อหนึ่งค่าที่ระบุ
แต่ละ column เก็บ aggregate ของแถวที่ `column` มีค่านั้น `group` ก่อน `pivot` กำหนดว่าแถวของผลลัพธ์คืออะไร

```biggo
let wide = sales
  |> group(product)
  |> pivot(region, ["north", "south", "east"], sum(qty))
  |> sort(product)
print(wide)

// หลาย aggregate ต้องตั้งชื่อ ชื่อ column คือ ค่า_ชื่อ
sales
  |> group(month = month(date))
  |> pivot(product, ["widget", "gadget"], orders = count(), best = max(price))
  |> sort(month)
  |> print()
```

```text output
+---------+-------+-------+------+
| product | north | south | east |
+---------+-------+-------+------+
| gadget  | 3     | 5     | 1    |
| gizmo   | 1     | null  | 2    |
| widget  | 17    | 12    | 4    |
+---------+-------+-------+------+
+-------+---------------+---------------+-------------+-------------+
| month | widget_orders | gadget_orders | widget_best | gadget_best |
+-------+---------------+---------------+-------------+-------------+
| 1     | 2             | 1             | 2.5         | 10.0        |
| 2     | 1             | 1             | 2.5         | null        |
| 3     | 1             | 1             | 2.5         | 10.0        |
| 12    | 1             | 0             | 2.5         | null        |
+-------+---------------+---------------+-------------+-------------+
```

ต้องระบุค่าที่จะกลายเป็น column เองเพราะ type ของตาราง (รายชื่อ column) ต้องรู้ตอน compile
ค่าที่ไม่อยู่ในรายการถูกข้าม ค่าที่อยู่ในรายการแต่ไม่มีข้อมูลได้ null (หรือ `0` สำหรับ `count`)
หารายการค่าได้ด้วย `distinct(region)` `first` และ `last` ใช้ใน `pivot` ไม่ได้

### unpivot: column → แถว

`unpivot(a, b, ...)` ทำกลับกัน: แต่ละแถวกลายเป็นหลายแถว แถวละหนึ่ง column ที่ระบุ
ได้ column ใหม่สองตัวคือชื่อของ column เดิม และค่าของมัน

```biggo
let wide = sales |> group(product) |> pivot(region, ["north", "south"], sum(qty))
print(wide |> unpivot(north, south, names = "region", values = "qty") |> sort(product, region))
```

```text output
+---------+--------+------+
| product | region | qty  |
+---------+--------+------+
| gadget  | north  | 3    |
| gadget  | south  | 5    |
| gizmo   | north  | 1    |
| gizmo   | south  | null |
| widget  | north  | 17   |
| widget  | south  | 12   |
+---------+--------+------+
```

- column ที่ระบุต้องเป็นชนิดเดียวกัน
- `names` และ `values` ตั้งชื่อ column ใหม่ (ค่าเริ่มต้น `"name"` และ `"value"`) ต้องเขียนเป็น string ตรง ๆ
- column ที่ไม่ได้ระบุถูกคงไว้และซ้ำค่าในทุกแถวที่แตกออกมา

### explode: หนึ่งแถว → หลายแถว

`explode(column)` แยก string ที่มีหลายค่าคั่นด้วย `,` ออกเป็นแถวละค่า ตัดช่องว่างรอบแต่ละค่า
argument ที่สองเปลี่ยนตัวคั่นได้

```biggo
let people = from_rows([
  { name: "Ann", langs: "en, th,ja" },
  { name: "Bo", langs: "th" },
  { name: "Cy", langs: null },
])
print(people |> explode(langs))
print(people |> explode(langs) |> group(langs) |> agg(speakers = count()) |> sort(langs))
print(from_rows([{ path: "usr/local/bin" }]) |> explode(path, "/"))
```

```text output
+------+-------+
| name | langs |
+------+-------+
| Ann  | en    |
| Ann  | th    |
| Ann  | ja    |
| Bo   | th    |
| Cy   | null  |
+------+-------+
+-------+----------+
| langs | speakers |
+-------+----------+
| en    | 1        |
| ja    | 1        |
| th    | 2        |
| null  | 1        |
+-------+----------+
+-------+
| path  |
+-------+
| usr   |
| local |
| bin   |
+-------+
```

แถวที่ค่าเป็น null คงเป็นหนึ่งแถวที่ค่าเป็น null

## สถิติ

```biggo
let points = from_rows([
  { x: 1, y: 2.1 },
  { x: 2, y: 3.9 },
  { x: 3, y: 6.2 },
  { x: 4, y: 7.8 },
  { x: 5, y: 10.1 },
])

// สหสัมพันธ์และเส้นถดถอย y = slope * x + intercept
print(points |> agg(r = corr(y, x), slope = slope(y, x), intercept = intercept(y, x)))

// linreg ให้ record ของเส้นถดถอย พร้อม r²
let line = linreg(points, y, x)
print(line)
print((line.slope ?? 0.0) * 6 + (line.intercept ?? 0.0))      // ทำนาย y ที่ x = 6
```

```text output
+--------------------+--------------------+---------------------+
| r                  | slope              | intercept           |
+--------------------+--------------------+---------------------+
| 0.9986517555689657 | 1.9899999999999998 | 0.05000000000000071 |
+--------------------+--------------------+---------------------+
{slope: 1.9899999999999998, intercept: 0.05000000000000071, r2: 0.9973053289009772}
11.989999999999998
```

- `corr(y, x)` สัมประสิทธิ์สหสัมพันธ์ของ Pearson; `cov(y, x)` ความแปรปรวนร่วมของตัวอย่าง
- `slope(y, x)` และ `intercept(y, x)` คือเส้นกำลังสองน้อยที่สุด: **ตัวแปรตามมาก่อน**
- แถวที่ `x` หรือ `y` เป็น null ถูกข้าม ถ้าเหลือน้อยกว่า 2 แถว หรือ `x` ไม่มีการกระจาย ผลเป็น null
- ทั้งสี่ตัวเป็น aggregate ใช้ร่วมกับ `group` ได้ เพื่อหาความสัมพันธ์แยกตามกลุ่ม
- `linreg(t, y, x)` ได้ record `{ slope, intercept, r2 }` ทุก field เป็น `float?`

`describe` สรุปทุก column ในการอ่านข้อมูลรอบเดียว และ `histogram` นับการกระจายของ column ตัวเลข:

```biggo
print(describe(sales))
print(histogram(sales, qty, bins = 4))
```

```text output
+---------+--------+-------+-------+--------------------+-------------------+------+--------+-------+
| column  | type   | count | nulls | mean               | stddev            | min  | median | max   |
+---------+--------+-------+-------+--------------------+-------------------+------+--------+-------+
| date    | date   | 10    | 0     | null               | null              | null | null   | null  |
| region  | string | 10    | 0     | null               | null              | null | null   | null  |
| product | string | 10    | 0     | null               | null              | null | null   | null  |
| qty     | int    | 10    | 0     | 4.5                | 4.034572812303401 | 0.0  | 3.5    | 12.0  |
| price   | float? | 9     | 1     | 25.822222222222223 | 42.14584136595739 | 2.5  | 2.5    | 100.0 |
+---------+--------+-------+-------+--------------------+-------------------+------+--------+-------+
+-----------+---------+-------+
| bin_start | bin_end | count |
+-----------+---------+-------+
| 0.0       | 3.0     | 4     |
| 3.0       | 6.0     | 3     |
| 6.0       | 9.0     | 1     |
| 9.0       | 12.0    | 2     |
+-----------+---------+-------+
```

- `describe(t)` ได้ตารางที่มีหนึ่งแถวต่อ column: ชนิด จำนวนค่า จำนวน null และสำหรับ column ตัวเลข
  ค่าเฉลี่ย ส่วนเบี่ยงเบนมาตรฐาน ค่าต่ำสุด มัธยฐาน ค่าสูงสุด
- `histogram(t, column)` แบ่งช่วงระหว่างค่าต่ำสุดกับสูงสุดเป็น 10 ช่วงกว้างเท่ากัน (`bins = n` เปลี่ยนจำนวน)
  แล้วนับค่าในแต่ละช่วง ค่าสูงสุดอยู่ในช่วงสุดท้าย null ไม่ถูกนับ
- ทั้งสองคืนตารางธรรมดา นำไป `where` `sort` หรือเขียนไฟล์ต่อได้

## ตารางกับ list ของ record

`to_rows` ดึงผลของ query ออกมาเป็น `list` ของ record เพื่อใช้ในโค้ดทั่วไป
และ `from_rows` สร้างตารางจาก `list` ของ record

```biggo
let top = to_rows(sales |> sort(desc(qty)) |> take(2) |> select(product, qty))
print(top)
print(top[0].product, map(top, fn(row) { row.qty * 2 }))

each(top, fn(row) {
  print(row.product, "ขายได้", row.qty)
})

let cities = from_rows([
  { city: "Bangkok", people: 10.5 },
  { city: "Chiang Mai", people: null },
])
print(cities |> where(not is_null(people)))

// list ว่างไม่มี type ของแถว ต้องบอกเอง
print(from_rows<{ id: int, note: string? }>([]))
```

```text output
[{product: "widget", qty: 12}, {product: "widget", qty: 10}]
widget [24, 20]
widget ขายได้ 12
widget ขายได้ 10
+---------+--------+
| city    | people |
+---------+--------+
| Bangkok | 10.5   |
+---------+--------+
+----+------+
| id | note |
+----+------+
+----+------+
```

`to_rows` โหลดทุกแถวเข้าหน่วยความจำเป็นค่าของตัวแปลภาษา จึงเหมาะกับผลลัพธ์ขนาดเล็ก
(ผลสรุป, top-n, ค่า config) งานกับข้อมูลจำนวนมากควรอยู่ในรูป operation ของตารางให้นานที่สุด
ตัวอย่างการใช้ผลของ query หนึ่งเป็นค่าของอีก query:

```biggo
let average = to_rows(sales |> agg(m = mean(qty)))[0].m ?? 0.0
print(average)
print(sales |> where(qty > average) |> select(product, qty))
```

```text output
4.5
+---------+-----+
| product | qty |
+---------+-----+
| widget  | 10  |
| gadget  | 5   |
| widget  | 7   |
| widget  | 12  |
+---------+-----+
```

## ฟังก์ชันที่รับและคืนตาราง

ขั้นตอนที่ใช้ซ้ำเขียนเป็นฟังก์ชันธรรมดาที่รับและคืนตาราง แล้วใช้ใน pipeline ได้เหมือน built-in

```biggo
type Priced = table<{ product: string, qty: int, price: float? }>
type Revenue = table<{ product: string, revenue: float }>

fn revenue_by_product(t: Priced, min_qty: int) -> Revenue {
  t
    |> where(qty >= min_qty)
    |> group(product)
    |> agg(revenue = sum(qty * (price ?? 0.0)))
}

sales
  |> select(product, qty, price)
  |> revenue_by_product(2)
  |> sort(desc(revenue))
  |> print()
```

```text output
+---------+---------+
| product | revenue |
+---------+---------+
| gizmo   | 199.8   |
| widget  | 82.5    |
| gadget  | 30.0    |
+---------+---------+
```

ฟังก์ชันแบบนี้ไม่ทำให้ช้าลง: มันถูกเรียกครั้งเดียวเพื่อ *สร้างแผน* แล้วแผนทั้งหมดถูกปรับรวมกัน
เหมือนเขียนต่อกันตรง ๆ

## ลำดับของแถวและผลลัพธ์ที่ทำซ้ำได้

engine ทำงานหลาย core แต่ **ผลลัพธ์เหมือนกันทุกครั้ง ไม่ว่าเครื่องจะมีกี่ core**:

- operation ที่ไม่เรียง (`where` `select` `derive` `join` ...) คงลำดับของแถวตามข้อมูลต้นทาง
- `group` ให้กลุ่มตามลำดับที่พบครั้งแรก
- `sort` เป็น stable sort
- ผลรวมของ `float` ถูกบวกตามลำดับที่ตายตัว จึงได้เลขเดิมทุกหลัก ไม่ว่าใช้กี่ thread

จำนวน thread กำหนดด้วย environment variable `RAYON_NUM_THREADS` (ค่าเริ่มต้นคือจำนวน core)
