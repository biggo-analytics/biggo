# biggo

> **biggo** is a statically typed, pipeline-oriented programming language for data analytics.
> Its compiler, bytecode VM and multi-threaded columnar engine (on Apache Arrow) are written in
> Rust and ship as one executable, together with a REPL, formatter, test runner and language
> server. The documentation is in Thai; the [roadmap](ROADMAP.md) lists what comes next.

biggo เป็นภาษาโปรแกรมสำหรับงานวิเคราะห์ข้อมูล พร้อม compiler และ engine ที่เขียนด้วย Rust
โปรแกรมอ่านข้อมูลเข้ามาเป็นตาราง แปลงด้วย pipeline แล้วพิมพ์หรือเขียนผลออกไป
ทุกอย่างถูกตรวจ type ก่อนรัน รวมถึงชื่อและชนิดของทุก column และ query ถูกรันโดย engine
แบบ column ที่ใช้ทุก core ของเครื่อง

**สถานะ:** รุ่น 0.1.0 ซึ่งเป็นรุ่นแรก ใช้งานได้ตามที่เอกสารเขียน แต่ตัวภาษายังเปลี่ยนได้ และทดสอบบน macOS
(Apple Silicon) เท่านั้น ดู[ข้อจำกัด](#ข้อจำกัดที่ควรรู้)และ[แผนการพัฒนา](ROADMAP.md)

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }

read_csv<Sale>("docs/data/sales.csv")
  |> where(qty > 0 and date >= @2026-01-01)
  |> derive(revenue = qty * (price ?? 0.0))
  |> group(region, month = month(date))
  |> agg(total = sum(revenue), orders = count())
  |> sort(desc(total))
  |> take(3)
  |> print()
```

```text output
+--------+-------+-------+--------+
| region | month | total | orders |
+--------+-------+-------+--------+
| east   | 2     | 199.8 | 1      |
| north  | 3     | 100.0 | 1      |
| north  | 1     | 55.0  | 2      |
+--------+-------+-------+--------+
```

## จุดเด่น

- **ผิดแล้วรู้ก่อนรัน** — static typing ทั้งภาษา: ชื่อ column ผิด ชนิดไม่ตรง หรือลืมจัดการ null
  เป็น compile error พร้อมตำแหน่งและคำแนะนำ ก่อนที่จะอ่านข้อมูลสักแถว
- **เร็ว** — query บน CSV 5 ล้านแถวใช้ราว 0.2 วินาทีบนโน้ตบุ๊ก 8 core (ระดับเดียวกับ DuckDB และ
  Polars) เพราะคำนวณทีละ column บน Apache Arrow, มี optimizer, และขนานทุกขั้น
- **ผลลัพธ์ทำซ้ำได้** — ได้ผลเหมือนกันทุก bit ไม่ว่าใช้กี่ thread
- **ภาษาเล็กแต่ครบ** — ฟังก์ชัน, lambda, `match`, record, map, list, `import`,
  `decimal` สำหรับเงิน, `datetime` และ `duration` สำหรับเวลา
- **อ่านเขียนได้หลายรูปแบบ** — CSV, Parquet, JSON Lines, SQLite
- **เครื่องมือครบในไฟล์เดียว** — ตัวรัน, REPL, formatter, ตัวรัน test, language server,
  ตัวสร้าง executable แบบ standalone และ extension ของ VS Code

## เริ่มต้น

ต้องมี Rust (ทดสอบกับ 1.99) และ C compiler ของระบบ

```sh
git clone https://github.com/biggo-analytics/biggo.git && cd biggo
cargo build --release
cp target/release/biggo ~/.local/bin/      # หรือที่ใดก็ได้ใน PATH

biggo run hello.bgo          # รันโปรแกรม
biggo repl                   # ลองทีละบรรทัด
biggo check hello.bgo        # ตรวจ type อย่างเดียว
```

อ่านต่อ: [เริ่มต้นใช้งาน](docs/01-getting-started.md)

## ภาษาโดยสังเขป

โค้ดทั่วไป — ฟังก์ชัน, lambda, `match`, record และ map:

```biggo
fn tier(score: int?) -> string {
  match score {
    null => "ไม่มีคะแนน"
    100 => "เต็ม"
    _ => if (score ?? 0) >= 50 { "ผ่าน" } else { "ไม่ผ่าน" }
  }
}

let students = [
  { name: "Ann", score: 100 },
  { name: "Bo", score: 45 },
  { name: "Cy", score: null },
]
each(students, fn(s) { print(s.name, tier(s.score)) })

let passed = filter(students, fn(s) { (s.score ?? 0) >= 50 })
print(map(passed, fn(s) { s.name }), len(passed))
```

```text output
Ann เต็ม
Bo ไม่ผ่าน
Cy ไม่มีคะแนน
["Ann"] 1
```

เงินและเวลา — `decimal` ไม่มีค่าคลาดเคลื่อนของทศนิยม, `datetime` ลบกันได้ `duration`:

```biggo
print(0.1 + 0.2, 0.1d + 0.2d)

let start = @2026-01-31T22:30:00
let stop = @2026-02-01T01:15:30
print(stop - start, start + hours(36), to_date(stop))
```

```text output
0.30000000000000004 0.3
02:45:30 2026-02-02T10:30:00 2026-02-01
```

ตาราง — join, window, pivot และสถิติ:

```biggo
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
let sales = read_csv<Sale>("docs/data/sales.csv")

// ยอดต่อสินค้า แยก column ตามภูมิภาค
print(sales |> group(product) |> pivot(region, ["north", "south", "east"], sum(qty)) |> sort(product))

// สินค้าขายดีที่สุดของแต่ละภูมิภาค
sales
  |> window(by = region, order = desc(qty), place = row_number())
  |> where(place == 1)
  |> select(region, product, qty)
  |> sort(region)
  |> print()

print(describe(sales |> select(qty, price)))
```

```text output
+---------+-------+-------+------+
| product | north | south | east |
+---------+-------+-------+------+
| gadget  | 3     | 5     | 1    |
| gizmo   | 1     | null  | 2    |
| widget  | 17    | 12    | 4    |
+---------+-------+-------+------+
+--------+---------+-----+
| region | product | qty |
+--------+---------+-----+
| east   | widget  | 4   |
| north  | widget  | 10  |
| south  | widget  | 12  |
+--------+---------+-----+
+--------+--------+-------+-------+--------------------+-------------------+-----+--------+-------+
| column | type   | count | nulls | mean               | stddev            | min | median | max   |
+--------+--------+-------+-------+--------------------+-------------------+-----+--------+-------+
| qty    | int    | 10    | 0     | 4.5                | 4.034572812303401 | 0.0 | 3.5    | 12.0  |
| price  | float? | 9     | 1     | 25.822222222222223 | 42.14584136595739 | 2.5 | 2.5    | 100.0 |
+--------+--------+-------+-------+--------------------+-------------------+-----+--------+-------+
```

type ของตารางถูกติดตามตลอด pipeline:

```biggo error
type Sale = { date: date, region: string, product: string, qty: int, price: float? }
read_csv<Sale>("docs/data/sales.csv")
  |> group(region)
  |> agg(units = sum(qty))
  |> where(product == "widget")
  |> print()
```

```text output
error: undefined name `product`; the table has columns region, units
 --> example.bgo:5:12
  |
5 |   |> where(product == "widget")
  |            ^^^^^^^
```

หลัง `agg` ตารางเหลือแค่ `region` กับ `units`: compiler รู้ และบอกตั้งแต่ยังไม่ได้รัน

test เขียนด้วยภาษาเดียวกัน (`biggo test` รันทุกฟังก์ชัน `test_...` ในไฟล์ `*_test.bgo`):

```biggo
fn total(amounts: list<float>) -> float { fold(amounts, 0.0, fn(sum, x) { sum + x }) }

fn test_total() {
  assert_eq(total([1.5, 2.5]), 4.0)
  assert_eq(total([]), 0.0)
}
test_total()
print("ok")
```

```text output
ok
```

## เครื่องมือ

| คำสั่ง | หน้าที่ |
| --- | --- |
| `biggo run file.bgo` | รันโปรแกรม |
| `biggo repl` | โหมดโต้ตอบ |
| `biggo check file.bgo` | รายงาน syntax และ type error โดยไม่รัน |
| `biggo explain file.bgo` | แสดงแผนของ query ก่อนและหลัง optimize โดยไม่รัน |
| `biggo test [path]` | รัน test ในไฟล์ `*_test.bgo` |
| `biggo fmt [--check] files` | จัดรูปแบบโค้ด |
| `biggo build file.bgo -o app` | สร้าง executable ที่รันได้เอง |
| `biggo lsp` | language server สำหรับ editor (error ขณะพิมพ์, hover, format) |

extension ของ VS Code อยู่ใน [`editors/vscode`](editors/vscode)

## ประสิทธิภาพ

5 ล้านแถว, Apple M1 Pro 8 core, เวลาเป็นวินาที (ของ biggo รวมเวลาเปิดโปรแกรม):

| query | biggo | DuckDB 1.5.6 | Polars 2.0.0 |
| --- | --- | --- | --- |
| กรอง + จัดกลุ่ม (CSV) | 0.21 | 0.27 | 0.15 |
| จัดกลุ่ม 1 ล้านกลุ่ม (CSV) | 0.38 | 0.29 | 0.14 |
| join + จัดกลุ่ม (CSV) | 0.24 | 0.27 | 0.13 |
| top 5 จากค่าที่คำนวณ (CSV) | 0.24 | 0.27 | 0.14 |
| กรอง + จัดกลุ่ม (Parquet) | 0.06 | 0.02 | 0.02 |
| pivot (CSV) | 0.24 | 0.27 | 0.15 |
| window 1 ล้าน partition (CSV) | 0.47 | 0.47 | 0.45 |
| กรอง + จัดกลุ่ม (JSON Lines) | 0.43 | 0.19 | 0.56 |
| นับค่าไม่ซ้ำ (CSV) | 0.22 | 0.30 | 0.19 |
| ผลรวม decimal (CSV) | 0.18 | 0.25 | 0.14 |

วิธีวัด ผลแบบละเอียด การขยายตาม thread ความเร็วของ VM และจุดที่ยังช้า:
[ประสิทธิภาพ](docs/09-performance.md)

## เอกสาร

| หน้า | เนื้อหา |
| --- | --- |
| [เริ่มต้นใช้งาน](docs/01-getting-started.md) | build, โปรแกรมแรก, REPL |
| [ตัวภาษา](docs/02-language.md) | ไวยากรณ์, ตัวแปร, ฟังก์ชัน, lambda, `if`, `match`, list, record, map, `import` |
| [ระบบ type](docs/03-types.md) | ชนิดข้อมูลทั้งหมด, nullable, การแปลงชนิด, การอนุมาน type |
| [การทำงานกับตาราง](docs/04-tables.md) | `where` `select` `group` `agg` `join` `window` `pivot` สถิติ ฯลฯ |
| [แหล่งข้อมูล](docs/05-data-sources.md) | CSV, Parquet, JSON, SQLite |
| [built-in reference](docs/06-builtins.md) | ฟังก์ชันสำเร็จรูปทุกตัว |
| [เครื่องมือ](docs/07-tools.md) | คำสั่งทั้งหมด, `biggo test`, formatter, editor |
| [สถาปัตยกรรม](docs/08-architecture.md) | compiler และ engine ทำงานอย่างไร, วิธีเพิ่มความสามารถ |
| [ประสิทธิภาพ](docs/09-performance.md) | benchmark, วิธีวัด, ข้อจำกัด |
| [grammar](docs/10-grammar.md) | ไวยากรณ์แบบเป็นทางการ |

โปรแกรมตัวอย่างทุกอันในเอกสารถูกรันตอน `cargo test` และผลลัพธ์ที่แสดงถูกเทียบกับของจริง

## โครงสร้างโปรเจกต์

```text
crates/
  biggo-syntax/   lexer, parser, AST, diagnostic
  biggo-types/    ตัวตรวจ type, HIR
  biggo-plan/     schema, logical plan, optimizer
  biggo-exec/     execution engine บน Apache Arrow
  biggo-eval/     bytecode compiler, virtual machine, session
  biggo-fmt/      formatter
  biggo-lsp/      language server
  biggo-cli/      คำสั่ง biggo
ROADMAP.md        แผนการพัฒนา
docs/             เอกสาร และข้อมูลตัวอย่าง
editors/vscode/   extension ของ VS Code
bench/            benchmark และสคริปต์เทียบกับ DuckDB / Polars / CPython
testdata/         โปรแกรมตัวอย่างและผลลัพธ์ที่คาดหวัง (golden)
```

## การพัฒนา

```sh
cargo test                     # unit test, golden, เอกสาร, CLI
BIGGO_BLESS=1 cargo test       # เขียนผลลัพธ์ที่คาดหวังใหม่ (แล้วอ่าน diff)
cargo clippy --all-targets
cargo fmt
python3 bench/run.py           # benchmark (ต้อง build --release และสร้างข้อมูลก่อน)
```

## ข้อจำกัดที่ควรรู้

- ข้อมูลของ query ที่ต้องเรียงทั้งตาราง, `window`, ฝั่งขวาของ `join` และ `group` ที่กลุ่มเยอะ
  ต้องพอดีกับ RAM (ยังไม่มีการเขียนลงดิสก์ชั่วคราว)
- ฐานข้อมูลที่ต่อได้มีแต่ SQLite; CSV คั่นด้วย `,` เท่านั้น; JSON ต้องเป็นหนึ่ง object ต่อบรรทัด
- ไม่มีลูป `for`/`while` (ใช้ `map` `filter` `fold` `each` หรือ recursion), ไม่มีการแก้ค่าของตัวแปร
- ฟังก์ชันที่เขียนเองรับ column ไม่ได้: ใน expression ของ column ใช้ได้แต่ตัวดำเนินการและ built-in
- ฟังก์ชันของ string ยังมีน้อย (ไม่มี regular expression, substring, replace)
- โปรแกรมรับ argument จาก command line ไม่ได้
- language server ยังไม่มี autocomplete และ go to definition; extension ของ VS Code
  ถูก package และทดสอบ grammar แล้ว แต่ยังไม่ได้ลองรันใน VS Code จริง
- ทดสอบบน macOS (Apple Silicon) เท่านั้น

## แผนการพัฒนา

ลำดับงานถัดไป เรียงตามความสำคัญ ไม่มีกำหนดวัน รายละเอียด เหตุผล และเกณฑ์ว่าเสร็จของแต่ละรุ่นอยู่ใน
[ROADMAP.md](ROADMAP.md)

| รุ่น | เป้าหมาย | งานหลัก |
| --- | --- | --- |
| 0.2 | ติดตั้งง่าย ใช้ได้ทุกระบบ | CI บน Linux / macOS / Windows, ไฟล์ติดตั้งสำเร็จรูป, extension ใน Marketplace, argument ของโปรแกรม, ฟังก์ชัน string และ regular expression, ตัวเลือกของ CSV |
| 0.3 | แหล่งข้อมูลมากขึ้น | PostgreSQL และ MySQL, Excel, หลายไฟล์ด้วย `*`, ไฟล์บีบอัด, `https://` และ S3, column แบบซ้อน |
| 0.4 | engine เร็วขึ้น และรับข้อมูลใหญ่กว่า RAM | ข้าม row group ของ Parquet, `group` ที่กลุ่มเยอะ, `join` แบบขนาน, เขียนลงดิสก์ชั่วคราว |
| 0.5 | ภาษาที่ใช้ซ้ำได้มากขึ้น | ฟังก์ชันที่เขียนเองใช้กับ column ได้, ฟังก์ชันที่รับตารางได้หลายรูป, generic, `import ... as` |
| 1.0 | เสถียร | ตรึงตัวภาษา, fuzzing, เทียบผลกับ DuckDB |

พบ bug หรืออยากเสนอความสามารถ เปิด issue ได้ที่
<https://github.com/biggo-analytics/biggo/issues>
