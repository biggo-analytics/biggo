# สถาปัตยกรรม

หน้านี้อธิบายว่า `biggo` ทำงานอย่างไรข้างใน สำหรับคนที่จะอ่านหรือแก้ source code
ตัว compiler และ engine เขียนด้วย Rust ทั้งหมด แบ่งเป็น 8 crate ใน `crates/`

## ภาพรวม

โปรแกรม biggo มีสองโลกที่ทำงานต่างกันมาก:

- **โค้ดทั่วไป** (ตัวแปร ฟังก์ชัน `if` list) ถูกแปลเป็น bytecode แล้วรันบน virtual machine ทีละคำสั่ง
- **ตาราง** ถูกแปลเป็น *แผน* (logical plan) ซึ่งถูกปรับแล้วรันโดย engine ที่ทำงานทีละ column
  บนทุก core

ตัวตรวจ type เป็นตัวแยกสองโลกนี้ออกจากกันตั้งแต่ตอน compile

```text
source (.bgo)
   │  lexer + parser                          biggo-syntax
   ▼
  AST ───────────────► formatter              biggo-fmt
   │  type checker                            biggo-types
   ▼
  HIR  (ชื่อถูกผูกแล้ว, type ครบ, table operation มี plan node แนบมา)
   │  bytecode compiler                       biggo-eval
   ▼
bytecode ── VM ──► ค่า (int, string, list, record, closure, ...)
              │
              │ table operation สร้าง plan node
              ▼
        logical plan                          biggo-plan
              │  optimizer
              ▼
        optimized plan
              │  execution engine             biggo-exec
              ▼
        Arrow record batches ──► print / write / to_rows
```

| crate | หน้าที่ | ขนาดโดยประมาณ |
| --- | --- | --- |
| `biggo-syntax` | span, token, lexer, parser, AST, diagnostic, รูปข้อความของ decimal/datetime | 2,800 บรรทัด |
| `biggo-types` | ผูกชื่อ, ตรวจ type, อนุมาน schema, แปลง AST เป็น HIR | 5,200 |
| `biggo-plan` | schema, expression ของ column, logical plan, optimizer | 2,100 |
| `biggo-exec` | engine: อ่าน/เขียนไฟล์, filter, aggregate, join, sort, window | 4,100 |
| `biggo-eval` | bytecode compiler, VM, `Session` (รวมถึง `import`) | 2,800 |
| `biggo-fmt` | formatter | 800 |
| `biggo-lsp` | language server | 300 |
| `biggo-cli` | คำสั่ง `biggo`: run, repl, check, test, build, ... | 600 |

การพึ่งพาเป็นทางเดียว: `syntax` ← `plan` ← `types` ← `eval` → `exec`
`biggo-types` ไม่รู้จัก engine เลย (รู้แค่รูปของแผน) และ `biggo-exec` ไม่รู้จักตัวภาษาเลย
(รับแผนเข้า คืน Arrow batch ออก) dependency ภายนอกหลักคือ `arrow` / `parquet` (รูปแบบข้อมูลและ
kernel คำนวณ), `rayon` (thread pool), `rusqlite` (SQLite ฝังในตัว), `memmap2`, `hashbrown`

## front end: จากข้อความเป็น AST

**Lexer** (`lexer.rs`) เขียนมือ อ่าน byte ทีละตัว แต่ละ token จำ span (ตำแหน่งเริ่มและจบเป็น `u32`)
และ flag `newline_before` ซึ่ง parser ใช้ตัดสินว่า statement จบหรือยัง comment ถูกเก็บแยกไว้
ให้ formatter

**Parser** (`parser.rs`) เป็น recursive descent สำหรับ statement และ Pratt parser สำหรับ expression
(แต่ละตัวดำเนินการมี "binding power") จุดออกแบบสำคัญ:

- **AST อยู่ใน arena**: expression ทุกตัวอยู่ใน `Vec` เดียวและอ้างถึงกันด้วย `ExprId` (เลข 32 บิต)
  pass ถัดไปจึงแนบข้อมูลกับ expression ได้ด้วยตารางที่ index ด้วย id — ตัวตรวจ type ใช้วิธีนี้
  เก็บ type ของทุก expression ให้ language server ตอบ hover
- **ชื่อถูก intern**: ชื่อแต่ละชื่อเป็น `Symbol` (เลข) เทียบกันได้ด้วยการเทียบเลข
- **กู้คืนจาก error**: เมื่อเจอ syntax error parser ข้ามไปถึงต้น statement ถัดไปแล้วทำต่อ
  จึงรายงานหลาย error ในรอบเดียว
- **จำกัดความลึก** 256 ชั้น เพื่อไม่ให้ input ประหลาดทำ stack ล้น

**Diagnostic** มีแค่ span กับข้อความ `SourceFile::render` วาดออกมาพร้อมบรรทัดของ source และ `^^^`
ทุก pass ตั้งแต่ lexer ถึง VM รายงาน error ด้วยโครงสร้างเดียวกันนี้

## ตัวตรวจ type

`biggo-types` ทำสามอย่างในรอบเดียว: ผูกชื่อกับที่อยู่ของค่า, ตรวจและอนุมาน type, และแปลง AST เป็น
**HIR** (`hir.rs`) ซึ่งเป็นรูปที่ compile ต่อได้ทันที:

- ชื่อทุกชื่อกลายเป็น `Local(slot)`, `Capture(index)`, `Global(slot)` หรือ `Function(id)`
- การแปลงชนิดอัตโนมัติถูกเขียนออกมาเป็น node (`ToFloat`, `Convert`)
- argument แบบระบุชื่อถูกจับคู่กับ parameter แล้ว
- table operation แต่ละตัวกลายเป็น `TableExpr`: plan node (`TableOp`) + ตารางที่เป็น input +
  ค่าจากโปรแกรมที่ expression ของ column อ้างถึง (parameter)

กลไกหลัก (`check.rs`):

- `unify(a, b)` หา type ที่ทั้งสองฝั่งแปลงไปหาได้ (ใช้กับ branch ของ `if`, สมาชิกของ list, ...)
- `coerce(expr, type)` แทรก node แปลงชนิดเมื่อการแปลงนั้นไม่เสียข้อมูล
- lambda รับ type ของ parameter จากบริบท (`FnHint`) ที่ส่งลงมาจากจุดที่มันถูกเขียน

### expression ของ column

ภายใน `where`, `derive`, `agg` ฯลฯ (`verbs.rs`) ตัวตรวจเปิด *scope ของ column*: ชื่อที่ตรงกับ column
ของตารางกลายเป็น node `Column` จากนั้น `lower` แปลง expression ที่ตรวจแล้วเป็นหนึ่งในสองอย่าง:

- ถ้าไม่มี column อยู่ข้างในเลย → คงเป็นโค้ดธรรมดา ซึ่ง VM จะคำนวณ **ครั้งเดียว** ตอนสร้างแผน
  แล้วส่งค่าเข้าแผนเป็น parameter
- ถ้ามี column → กลายเป็น `plan::Expr` ที่ engine คำนวณทั้ง column

กฎนี้คือเหตุที่ `where(qty > threshold())` เรียก `threshold()` ครั้งเดียว และเหตุที่ฟังก์ชันที่เขียนเอง
รับ column ไม่ได้ (ไม่มีรูปของ `plan::Expr` ให้มัน)

### สิ่งที่หายไปก่อนถึง engine (desugaring)

หลายอย่างในภาษาไม่มีตัวตนใน engine เพราะตัวตรวจ type แปลงเป็นของที่มีอยู่แล้ว:

| ที่เขียน | สิ่งที่ตัวตรวจสร้าง |
| --- | --- |
| `match x { a => p, _ => q }` | `if x == a { p } else { q }` (ค่าของ `x` เก็บในตัวแปรชั่วคราว) |
| `distinct()` | `group` ด้วยทุก column แล้ว `agg` ที่ไม่มี aggregate |
| `pivot(c, [v1, v2], sum(x))` | `agg(v1 = sum(if c == v1 { x } else { null }), v2 = ...)` |
| `linreg(t, y, x)` | `agg(slope(y, x), intercept(y, x), corr(y, x))` + `project` + ดึงแถวเดียว |
| join แบบ `"right"` | join แบบ `"left"` ที่สลับฝั่ง |

ข้อดีคือ optimizer และ engine ไม่ต้องรู้จักของใหม่: `pivot` ได้ filter pushdown และ column pruning
มาฟรี ๆ เพราะมันคือ aggregate ธรรมดา

## bytecode และ virtual machine

`biggo-eval/compile.rs` แปลง HIR ของแต่ละฟังก์ชันเป็น `Proto`: ลำดับของ `Op` พร้อมตารางค่าคงที่
VM (`vm.rs`) เป็น **stack machine**: แต่ละคำสั่งหยิบ operand จากยอด stack และวางผลลัพธ์กลับ
ตัวแปร local อยู่บน stack เดียวกันที่ตำแหน่งตายตัวนับจากฐานของ frame

- **`Value`** กว้าง 24 byte ค่าที่ใหญ่กว่าตัวเลขอยู่หลัง reference count (`Rc`/`Arc`) การ copy
  ค่าจึงถูกเสมอ และค่าทุกชนิดเปลี่ยนแปลงไม่ได้
- **คำสั่งเฉพาะทาง**: เมื่อตัวตรวจ type รู้ว่าทั้งสองฝั่งเป็น `int` compiler ออกคำสั่งอย่าง `AddInt`,
  `LtInt` ที่ไม่ต้องตรวจชนิดตอนรัน
- **closure** คือ id ของฟังก์ชันกับค่าที่จับมา (`captures`) ซึ่งถูก copy ตอนสร้าง closure
- **ลูป** (`map` `filter` `fold` `each`) ถูก compile เป็นคำสั่ง `LoopStart` / `LoopNext` /
  `LoopStep` / `LoopEnd` ในฟังก์ชันที่เรียกมัน ไม่ใช่การเรียกซ้อนในตัว VM ฟังก์ชันที่ลูปเรียก
  จึงเป็นการเรียกธรรมดาใน dispatch loop เดียวกัน: recursion ลึกผ่านลูปได้เท่ากับ recursion ปกติ
- **table operation** เป็นคำสั่ง `Table(site)`: หยิบตาราง input และค่า parameter จาก stack
  แล้วเรียก `TableOp::build` ได้ `Value::Table(Arc<Plan>)` — ยังไม่มีอะไรรัน
- **built-in ที่รัน query** (`print`, `write_*`, `count`, `to_rows`, ...) เรียกเข้า `biggo-exec`
- stack ของการเรียกลึกได้ 100,000 frame

`Session` (`lib.rs`) ร้อยทุกอย่างเข้าด้วยกันสำหรับ source หนึ่งชิ้น: parse → โหลดไฟล์ที่ `import`
(แต่ละไฟล์ครั้งเดียว, ตรวจจับ import วงกลม) → ตรวจ type → compile → รัน
session เดียวรับ source ได้หลายชิ้นต่อกัน โดยแต่ละชิ้นเห็น definition ของชิ้นก่อนหน้า:
REPL ก็คือ session ที่ป้อนทีละบรรทัด และ `biggo test` คือ session ที่รันไฟล์ test แล้วป้อน
`test_x()` ตามทีละตัว

## แผนและ optimizer

`Plan` (`plan.rs`) เป็นต้นไม้ของ node: `Scan`, `Memory`, `Filter`, `Project`, `Sort`, `Limit`,
`Group`, `Aggregate`, `Join`, `Window`, `Union`, `Unpivot`, `Explode` ทุก node รู้ schema
ของผลลัพธ์ตัวเอง

ก่อนรัน `optimize` (`optimize.rs`) เขียนแผนใหม่ตามลำดับนี้:

1. **simplify** — คำนวณ expression ที่ไม่มี column ให้เหลือค่าคงที่ (constant folding),
   ตัด filter ที่เป็น `true` เสมอ
2. **push filters** — ดัน `Filter` ลงไปให้ใกล้ข้อมูลที่สุด: ผ่าน `Project` (เขียน predicate ใหม่
   ด้วย expression ต้นทาง), ผ่าน `Aggregate` เมื่อใช้แต่ key ของกลุ่ม, ผ่าน `Window` เมื่อใช้แต่
   column ของ partition, ลงทั้งสองฝั่งของ `Join` ตามที่ชนิดของ join อนุญาต และสุดท้าย **เข้าไปใน
   `Scan`** ซึ่งกรองแถวตั้งแต่ตอนถอดรหัสไฟล์ predicate ที่ผิดพลาดได้ตอนรัน (เช่น `%`) ไม่ถูกย้าย
   ข้ามจุดที่จะทำให้มันเจอแถวที่โปรแกรมเดิมไม่เคยให้มันเห็น
3. **push limits** — `Sort` ที่ตามด้วย `Limit` กลายเป็น top-n (`fetch`), `Limit` เหนือ `Scan`
   ทำให้หยุดอ่านไฟล์เมื่อได้แถวครบ
4. **prune columns** — ไล่จากบนลงล่างว่าแต่ละ node ต้องใช้ column ไหนจริง แล้วตัดที่เหลือ
   จนถึง `Scan` ซึ่งจะไม่ถอดรหัส column ที่ไม่มีใครใช้
5. **merge projects** — `Project` ที่ซ้อนกันถูกรวมเป็นชั้นเดียว

ทุกกฎรักษาผลลัพธ์ให้เหมือนเดิมทุก bit รวมถึงลำดับของแถว `biggo explain` แสดงแผนก่อนและหลัง

## execution engine

`biggo-exec` รันแผนบน **Apache Arrow**: ตารางคือลำดับของ `RecordBatch` ซึ่งเก็บแต่ละ column เป็น
array ต่อเนื่องในหน่วยความจำ (batch ละ 32,768 แถว) operation ทำงานกับทั้ง array ด้วย kernel
ของ Arrow ซึ่ง compiler ของ Rust แปลงเป็นคำสั่ง SIMD ได้

**รูปแบบการรัน** — `execute(plan)` คืน iterator ของ batch (pull model) ผู้ใช้ที่หยุดดึงก่อน
(เช่น `take(5)`) ทำให้งานที่เหลือไม่เกิดขึ้น operation ที่ทำทีละ batch (`Filter`, `Project`,
`Unpivot`, `Explode`, ฝั่ง probe ของ join) ใช้ `par_map`: ดึง batch มาเท่าจำนวน core ประมวลผลพร้อมกัน
แล้วส่งออก **ตามลำดับเดิม**

**Scan** (`scan.rs`) — ไฟล์ถูกแบ่งเป็น "ชิ้น" ที่ถอดรหัสแยกกันได้: CSV และ JSON ตัดทุก 2 MiB ที่ขอบ
บรรทัด (CSV นับ `"` เพื่อไม่ตัดกลางค่าที่มีการขึ้นบรรทัดใหม่), Parquet แบ่งตาม row group
ชิ้นถูกถอดรหัสครั้งละเท่าจำนวน core และตัวกรองของ scan ทำงานในชิ้นเลย ขนาดชิ้นตายตัวไม่ขึ้นกับ
จำนวน core เพื่อให้ batch ที่ไหลออกมาเหมือนกันทุกเครื่อง

**Aggregate** (`aggregate.rs`) — hash aggregation สองขั้น:

1. batch ถูกรวมเป็น "run" ละ 4 batch แต่ละ run ถูกสรุปบน core ของตัวเองเป็นผลบางส่วน:
   key ของกลุ่มถูกเข้ารหัสเป็น byte (row format ของ Arrow) แล้วใช้ hash table หาเลขกลุ่ม
   จากนั้น accumulator ของแต่ละ aggregate อัปเดตทั้ง batch ในลูปแน่น ๆ
2. ผลบางส่วนถูกรวม: ถ้ากลุ่มน้อยก็รวมทีละ run; ถ้ากลุ่มเกิน 50,000 จะแบ่งกลุ่มตาม hash ของ key
   เป็นส่วน ๆ แล้วรวมแต่ละส่วนบน core ของตัวเอง สุดท้ายเรียงกลุ่มกลับตามลำดับที่พบครั้งแรก

`count_distinct` ของชนิดที่กว้างคงที่เก็บค่าของแต่ละกลุ่มไว้เฉย ๆ แล้วเรียง + นับตอนจบบนทุก core
ซึ่งเร็วกว่าการดูแล hash set ต่อกลุ่มระหว่างทาง

**Join** (`join.rs`) — hash join: สร้าง hash table จากตารางขวาทั้งตาราง แล้ว probe ด้วย batch
ของตารางซ้ายแบบขนาน

**Sort** (`ops.rs`) — top-n ใช้การเลือกบางส่วนของ Arrow; การเรียงทั้งตารางเข้ารหัส key ของแต่ละแถว
เป็น byte ที่เทียบกันได้ตรง ๆ (ขนานกันเป็นช่วง) แล้วเรียงเลขแถวบนทุก core โดยพก 16 byte แรก
ของ key ไปกับเลขแถวเพื่อให้การเทียบส่วนใหญ่จบโดยไม่ต้องไปอ่าน key เต็ม แถวที่ key เท่ากันตัดสิน
ด้วยเลขแถว จึงเป็น stable sort ที่ได้ผลเดียวกันไม่ว่าจะแบ่งงานอย่างไร

**Window** (`window.rs`) — เรียงตาม partition และ order, หาขอบของ partition ด้วยการเทียบแถวติดกัน
แบบ vector, คำนวณแต่ละฟังก์ชันตามลำดับนั้น (ฟังก์ชันละ core) แล้ววางผลกลับตามลำดับแถวเดิม

### ผลลัพธ์ที่ทำซ้ำได้

ข้อกำหนดของ engine คือ **ผลลัพธ์ต้องเหมือนกันทุก bit ไม่ว่ามีกี่ thread** ซึ่งกำหนดการออกแบบหลายจุด:

- `par_map` คืน batch ตามลำดับ input เสมอ
- ขนาดของชิ้นไฟล์และของ run ใน aggregate เป็นค่าคงที่ ไม่ขึ้นกับจำนวน core ผลรวมของ `float`
  (ซึ่งขึ้นกับลำดับการบวก) จึงถูกบวกเป็นกลุ่มเดิมเสมอ
- กลุ่มของ `group` ออกมาตามลำดับที่พบครั้งแรก แม้จะรวมแบบแบ่งส่วน
- การเรียงตัดสินค่าที่เท่ากันด้วยเลขแถว

`bench/run.py` ตรวจเรื่องนี้ทุกครั้ง: รันแต่ละ query ด้วยทุก core และด้วย core เดียว แล้วเทียบ
ผลลัพธ์ที่พิมพ์ออกมา

### expression ที่ผิดพลาดได้

`plan::Expr::can_fail` บอกว่า expression หยุดโปรแกรมได้หรือไม่ (เช่น `%` ด้วยศูนย์, `int` ล้น)
branch ของ `if` และฝั่งขวาของ `and`/`or`/`??` ที่ผิดพลาดได้ถูกคำนวณเฉพาะแถวที่ต้องใช้ (`eval_where`)
เพื่อให้ `if n != 0 { x % n } else { 0 }` มีความหมายเดียวกับที่เขียน แม้ engine จะคำนวณทั้ง column
ส่วน expression ที่ผิดพลาดไม่ได้ถูกคำนวณทั้งสอง branch แล้วเลือก ซึ่งเร็วกว่า

## เครื่องมือ

- **formatter** (`biggo-fmt`) แปลง AST เป็น "เอกสาร" แบบ Wadler (`Doc`: ข้อความ, กลุ่ม, จุดที่ขึ้น
  บรรทัดได้) แล้วให้ตัวจัดวางเลือกว่ากลุ่มไหนพอดีบรรทัด comment ถูกวางกลับด้วยตำแหน่งใน source
- **language server** (`biggo-lsp`) เก็บข้อความของไฟล์ที่เปิดอยู่ วิเคราะห์ใหม่ทั้งไฟล์ทุกครั้งที่เปลี่ยน
  (ตัวตรวจเร็วพอ: หลายแสนบรรทัดต่อวินาที) และตอบ hover จากตาราง type ต่อ expression
- **`biggo build`** คัดลอก executable ของตัวเอง แล้วเขียน source ของโปรแกรมลงในบล็อก 256 KiB
  ที่จองไว้ใน binary (หาเจอด้วย marker) executable ที่พบโปรแกรมในบล็อกนี้จะรันมันแทนการอ่าน
  command line

## การทดสอบ

| ชนิด | อยู่ที่ | ตรวจอะไร |
| --- | --- | --- |
| unit test | `src/` ของแต่ละ crate | ส่วนย่อย: lexer, parser, VM, ตัวอ่านไฟล์, การเรียง |
| golden: syntax | `testdata/**/*.syntax` | syntax tree หรือ syntax error ของทุกไฟล์ตัวอย่าง |
| golden: run | `testdata/run/*.out` | ผลลัพธ์และ runtime error ของโปรแกรมตัวอย่าง |
| golden: check | `testdata/check/*.out` | type error ทุกข้อความ |
| golden: plan | `testdata/sales.plan` | แผนก่อนและหลัง optimize |
| ความทนทาน | `survives_mangled_programs` | ทุก prefix ของทุกโปรแกรม และทุกโปรแกรมที่ลบหนึ่งอักขระ ต้องไม่ panic |
| formatter | `biggo-fmt/tests` | จัดรูปแบบแล้วความหมายไม่เปลี่ยน และจัดซ้ำได้ผลเดิม กับทุกไฟล์ตัวอย่าง |
| เอกสาร | `biggo-cli/tests/docs.rs` | ทุกโปรแกรมในเอกสารรันได้ และผลลัพธ์ที่แสดงตรงกับของจริง |
| ความสอดคล้อง | `biggo-types/tests/in_sync.rs` | built-in ทุกตัวอยู่ใน grammar ของ editor และใน reference |
| CLI | `biggo-cli/tests/cli.rs` | คำสั่งจริงผ่าน process จริง: run, repl, fmt, test, build, lsp |

```sh
cargo test                      # ทั้งหมด
BIGGO_BLESS=1 cargo test        # เขียน golden และผลลัพธ์ในเอกสารใหม่ตามพฤติกรรมปัจจุบัน
cargo clippy --all-targets      # lint
cargo fmt                       # จัดรูปแบบโค้ด Rust
```

หลัง `BIGGO_BLESS=1` ต้องอ่าน diff ของไฟล์ที่เปลี่ยนเสมอ: bless คือการยืนยันว่าพฤติกรรมใหม่ถูกต้อง

## เพิ่มความสามารถ

**ฟังก์ชันของค่าเดี่ยว** (เช่น `replace(s, a, b)`):

1. เพิ่ม variant ใน `ScalarFn` พร้อมชื่อ (`biggo-plan/src/expr.rs`)
2. บอก type ของ argument และผลลัพธ์ใน `scalar_call` (`biggo-types/src/verbs.rs`)
3. เขียนการคำนวณบน Arrow array ใน `call` (`biggo-exec/src/expr.rs`)
4. เพิ่มชื่อใน grammar ของ editor และใน `docs/06-builtins.md` (test `in_sync` จะเตือนถ้าลืม)

ไม่ต้องแตะ VM: ค่าเดี่ยวถูกคำนวณผ่านเส้นทางเดียวกับ column (`call_scalar`) จึงได้พฤติกรรม
เดียวกันทั้งสองที่โดยอัตโนมัติ

**aggregate**: เพิ่มใน `AggFn`, บอก type ใน `agg_type`, เพิ่ม accumulator ใน `Acc`
(`new`, `update`, `merge`, `finish`) — `merge` ต้องให้ผลเหมือนการอัปเดตต่อเนื่อง

**table operation**: ถ้าเขียนในรูปของ operation ที่มีอยู่ได้ ให้ desugar ในตัวตรวจ type
(แบบ `pivot`) ถ้าไม่ได้ ให้เพิ่ม node ใน `Plan` และ `TableOp`, สอน optimizer (`map_plan`,
`push_filters`, `prune`) ว่า node ใหม่ต้องการ column อะไร, แล้วเขียนตัวรันใน `biggo-exec`

**built-in ของโค้ดทั่วไป** (แบบ `len`): เพิ่มใน `Builtin` (`hir.rs`), ตรวจ type ใน
`builtins.rs`, รันใน `Vm::builtin`
