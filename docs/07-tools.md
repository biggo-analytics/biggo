# เครื่องมือ

ทุกอย่างอยู่ใน executable ตัวเดียวชื่อ `biggo`: ตัวรัน ตัวตรวจ REPL formatter ตัวรัน test
ตัวสร้าง executable และ language server สำหรับ editor

```text
usage: biggo <command> [args]

commands:
  run <file>               run a program
  repl                     evaluate code interactively
  check <file>             report the syntax and type errors of a program
  explain <file>           show the query plans of a program without running them
  test [<path>...]         run the tests in the `*_test.bgo` files under the paths
  fmt [--check] <file>...  format programs in place, or list those that need it
  build <file> [-o <out>]  make a standalone executable of a program
  lsp                      serve an editor over the Language Server Protocol
  parse <file>             print the syntax tree of a program
  version                  print the version
```

| exit code | ความหมาย |
| --- | --- |
| 0 | สำเร็จ |
| 1 | โปรแกรมมี error (syntax, type, หรือตอนรัน), test ไม่ผ่าน, หรือ `fmt --check` เจอไฟล์ที่ยังไม่จัด |
| 2 | ใช้คำสั่งผิด |

ผลลัพธ์ของโปรแกรมออกทาง standard output ส่วน error ออกทาง standard error

## biggo run

```sh
biggo run report.bgo
```

รันโปรแกรม ก่อนรันจะตรวจ syntax และ type ของทั้งไฟล์ (และไฟล์ที่ `import`) ถ้าพบ error
จะรายงานทั้งหมดแล้วไม่รันอะไรเลย โปรแกรมที่ type ผิดจึงไม่มีทางรันไปครึ่งทางแล้วเขียนไฟล์ค้างไว้

path ของไฟล์ข้อมูลในโปรแกรมนับจากโฟลเดอร์ของไฟล์ `.bgo` ไม่ใช่โฟลเดอร์ที่สั่งรัน

จำนวน thread ที่ engine ใช้กำหนดด้วย `RAYON_NUM_THREADS` (ค่าเริ่มต้นคือจำนวน core ทั้งหมด):

```sh
RAYON_NUM_THREADS=2 biggo run report.bgo
```

## biggo check

```sh
biggo check bad.bgo
```

ตรวจ syntax และ type โดยไม่รัน ไม่แตะไฟล์ข้อมูล จึงเร็วและใช้ได้แม้ข้อมูลยังไม่พร้อม
รายงานทุก error ที่พบในรอบเดียว:

```text
error: cannot apply `>` to int and string
 --> bad.bgo:3:22
  |
3 | print(sales |> where(qty > "5"))
  |                      ^^^^^^^^^

error: the table has no column `amount`; its columns are region, qty
 --> bad.bgo:4:23
  |
4 | print(sales |> select(amount))
  |                       ^^^^^^

2 errors in bad.bgo
```

เหมาะกับการใส่ไว้ใน CI หรือ pre-commit hook คู่กับ `biggo fmt --check`

## biggo explain

```sh
biggo explain report.bgo
```

รันโปรแกรมในโหมดที่ **ไม่มี query ไหนถูกรันจริง**: ทุกจุดที่โปรแกรมจะพิมพ์ เขียน นับ หรือดึงแถวของตาราง
จะพิมพ์แผนของ query นั้นแทน ทั้งแผนตามที่เขียน (`plan`) และแผนหลังปรับ (`optimized plan`)
โค้ดส่วนที่ไม่ใช่ตารางรันตามปกติ ใช้ดูว่า

- ตัวกรองถูกดันลงไปถึงขั้นอ่านไฟล์หรือไม่ (`Scan ... where ...`)
- อ่าน column เท่าที่จำเป็นหรือไม่ (รายชื่อหลัง `Scan`)
- `sort` + `take` ถูกรวมเป็น top-n หรือไม่ (`Sort: qty desc (first 3)`)

ตัวอย่างผลลัพธ์อยู่ใน [เริ่มต้นใช้งาน](01-getting-started.md#ดูว่า-engine-จะทำอะไร)
ในโปรแกรมเองเรียก `explain(table)` เพื่อพิมพ์แผนของตารางเดียวได้

## biggo repl

```text
$ biggo repl
biggo 0.1.0 (Ctrl-D to exit)
>> type Sale = { region: string, qty: int }
>> let sales = read_csv<Sale>("docs/data/sales.csv")
>> sales |> group(region) |> agg(units = sum(qty))
+--------+-------+
| region | units |
+--------+-------+
| north  | 21    |
| south  | 17    |
| east   | 7     |
+--------+-------+
>> sales |> where(qty > "many")
error: cannot apply `>` to int and string
 --> <repl>:1:16
  |
1 | sales |> where(qty > "many")
  |                ^^^^^^^^^^^^
>> count(sales)
10
```

- แต่ละรายการถูกตรวจ type แล้วรันทันที ค่าของ expression ถูกแสดงในรูป literal (string มี `"`)
- `let` `fn` `type` และ `import` ที่ป้อนไว้ใช้ได้ต่อไปจนจบ session
- รายการที่ยังไม่จบ (วงเล็บไม่ปิด, จบด้วยตัวดำเนินการ) จะขึ้น prompt `..` รอบรรทัดต่อไป
  ป้อนบรรทัดว่างเพื่อส่งทั้งที่ยังไม่จบ
- error ไม่ทำให้ session จบ สิ่งที่ประกาศไว้ก่อนหน้ายังอยู่
- path ของไฟล์นับจากโฟลเดอร์ที่เปิด REPL
- รับ input จาก pipe ได้: `echo 'print(1 + 1)' | biggo repl` (ไม่มี prompt)

## biggo fmt

```sh
biggo fmt report.bgo other.bgo     # จัดรูปแบบแล้วเขียนทับไฟล์
biggo fmt --check *.bgo            # ไม่แก้ไฟล์ แค่พิมพ์ชื่อไฟล์ที่ยังไม่จัด (exit 1 ถ้ามี)
```

จัดรูปแบบโค้ดเป็นแบบมาตรฐานแบบเดียว ไม่มีตัวเลือกให้ตั้ง ก่อนและหลัง:

```text
type Sale={region:string,qty:int}
let   big=read_csv<Sale>("sales.csv")|>where(qty>5)   // large orders
   |>group(region)|>agg(units=sum(qty),orders=count())
print( big )
```

```text
type Sale = { region: string, qty: int }
let big = read_csv<Sale>("sales.csv")
  |> where(qty > 5)  // large orders
  |> group(region)
  |> agg(units = sum(qty), orders = count())
print(big)
```

กฎของรูปแบบ:

- เยื้อง 2 ช่องว่าง บรรทัดยาวไม่เกิน 100 ตัวอักษร
- pipeline ที่ไม่พอดีบรรทัด หรือที่ผู้เขียนขึ้นบรรทัดใหม่ไว้ ถูกจัดเป็นหนึ่งขั้นต่อบรรทัด
- argument ที่ไม่พอดีบรรทัดถูกจัดเป็นหนึ่งตัวต่อบรรทัด มี `,` ปิดท้าย
- lambda หลายบรรทัดที่เป็น argument สุดท้ายเริ่มบนบรรทัดเดียวกับการเรียก: `each(xs, fn(x) {`
- comment ถูกเก็บไว้ทุกอัน บรรทัดว่างเดี่ยวระหว่าง statement ถูกเก็บไว้ (หลายบรรทัดถูกยุบเหลือหนึ่ง)
- วงเล็บที่ไม่จำเป็นถูกเอาออก ยกเว้นที่ผู้เขียนใส่รอบ expression ประกอบเพื่อความชัดเจน
- literal ถูกคงไว้ตามที่เขียน (`1_000`, `1.50d`)

formatter รับประกันสองอย่าง ซึ่งถูกทดสอบกับทุกโปรแกรมตัวอย่างในโปรเจกต์:
ความหมายของโปรแกรมไม่เปลี่ยน (syntax tree และ comment เหมือนเดิม) และจัดซ้ำได้ผลเดิม
ไฟล์ที่มี syntax error ถูกรายงานและไม่ถูกแตะ

## biggo test

```sh
biggo test                  # ทุกไฟล์ *_test.bgo ใต้โฟลเดอร์ปัจจุบัน
biggo test tests/ lib/      # ใต้โฟลเดอร์ที่ระบุ
biggo test pricing_test.bgo # ไฟล์เดียว
```

test เขียนด้วยภาษา biggo เอง:

- **ไฟล์ test** คือไฟล์ที่ชื่อลงท้ายด้วย `_test.bgo`
- **test** คือฟังก์ชันระดับบนสุดที่ชื่อขึ้นต้นด้วย `test_` และไม่มี parameter
- test ผ่านเมื่อรันจนจบโดยไม่มี error ใช้ `assert` และ `assert_eq` ตรวจผล

สมมติมีไฟล์ `pricing.bgo`:

```biggo check
fn discount(amount: float, percent: float) -> float {
  amount * (1 - percent / 100)
}
```

ไฟล์ test `pricing_test.bgo` ที่อยู่ข้างกัน:

```biggo fragment
import "pricing.bgo"

fn test_no_discount() {
  assert_eq(discount(200, 0), 200.0)
}

fn test_half_price() {
  assert_eq(discount(200, 50), 100.0)
}

fn test_rounding() {
  print("discounted:", discount(19.99, 15))
  assert_eq(round(discount(19.99, 15), 2), 17.0)
}
```

```text
$ biggo test
ok    pricing_test.bgo::test_no_discount
ok    pricing_test.bgo::test_half_price
FAIL  pricing_test.bgo::test_rounding
      error: assertion failed: the values differ
        left:  16.99
        right: 17.0
        --> pricing_test.bgo:13:3
         |
      13 |   assert_eq(round(discount(19.99, 15), 2), 17.0)
         |   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
      output:
      discounted: 16.9915

2 passed, 1 failed
```

วิธีทำงาน:

1. แต่ละไฟล์ test ถูกรันทั้งไฟล์ก่อนหนึ่งครั้ง (เพื่อประกาศฟังก์ชันและตัวแปร) ถ้าขั้นนี้ล้มเหลว
   ทั้งไฟล์นับเป็นหนึ่ง test ที่ไม่ผ่าน
2. จากนั้นเรียกฟังก์ชัน `test_...` ทีละตัวตามลำดับที่ประกาศ test ที่ล้มเหลวไม่หยุด test ตัวอื่น
3. สิ่งที่ test พิมพ์ถูกเก็บไว้ และแสดงเฉพาะเมื่อ test นั้นไม่ผ่าน
4. ไฟล์ test ที่ไม่มีฟังก์ชัน `test_...` นับเป็นหนึ่ง test ที่ผ่านถ้ารันจนจบ
   (ใช้เขียน test แบบ script ที่มี `assert` ระดับบนสุด)
5. โฟลเดอร์ที่ชื่อขึ้นต้นด้วย `.` และโฟลเดอร์ `target`, `node_modules` ถูกข้าม

exit code เป็น 1 ถ้ามี test ไม่ผ่าน

### assert และ assert_eq

```biggo
assert(1 + 1 == 2)
assert(len([1, 2]) == 2, "ต้องมีสองตัว")           // ข้อความที่แสดงเมื่อไม่ผ่าน
assert_eq([1.0, 2.0], [1, 2])                      // เทียบลึกทั้งโครงสร้าง แปลงชนิดให้เหมือน ==
assert_eq({ name: "a", tags: ["x"] }, { name: "a", tags: ["x"] })
assert_eq(put({ "a": 1 }, "b", 2), { "b": 2, "a": 1 })   // map ไม่สนลำดับของ key

// ตาราง: column เหมือนกัน และแถวเหมือนกันตามลำดับ
let totals = from_rows([{ k: "a", v: 1 }, { k: "a", v: 2 }, { k: "b", v: 5 }])
  |> group(k)
  |> agg(total = sum(v))
assert_eq(totals, from_rows([{ k: "a", total: 3 }, { k: "b", total: 5 }]))
print("ผ่านทั้งหมด")
```

```text output
ผ่านทั้งหมด
```

เมื่อไม่ผ่าน error บอกทั้งสองค่า หรือสำหรับตาราง บอกแถวแรกที่ต่างกัน:

```biggo error
let got = from_rows([{ id: 1, name: "a" }, { id: 2, name: "b" }])
assert_eq(got |> sort(desc(id)), got)
```

```text output
error: assertion failed: row 1 differs
  left:  {id: 2, name: "b"}
  right: {id: 1, name: "a"}
 --> example.bgo:2:1
  |
2 | assert_eq(got |> sort(desc(id)), got)
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

`assert_eq` เทียบ `float` แบบตรงตัว: ค่าที่ผ่านการคำนวณควร `round` ก่อนเทียบ
และการเทียบตารางรัน query ทั้งสองฝั่งแล้วโหลดทุกแถว จึงเหมาะกับตารางขนาด test

## biggo build

```sh
biggo build report.bgo             # ได้ executable ชื่อ report
biggo build report.bgo -o bin/app  # ตั้งชื่อเอง
./report
```

สร้าง executable ที่รันโปรแกรมนั้นได้เองโดยไม่ต้องมี `biggo` หรือไฟล์ `.bgo` บนเครื่องปลายทาง

- executable คือสำเนาของ `biggo` ที่ฝัง source ของโปรแกรม **และของทุกไฟล์ที่มัน `import`** ไว้ข้างใน
  ขนาดจึงเท่ากับ `biggo` เอง (ราว 22 MB) ไม่ว่าโปรแกรมจะเล็กแค่ไหน
- โปรแกรมถูกตรวจ type ก่อน ถ้ามี error จะไม่สร้าง
- ตอนรัน source ถูก compile เป็น bytecode ใหม่ทุกครั้ง ซึ่งใช้เวลาระดับมิลลิวินาที
- path ของไฟล์ข้อมูลใน executable นับจาก **โฟลเดอร์ที่รัน** (ไม่มีไฟล์ `.bgo` ให้นับจากแล้ว)
- argument บน command line ถูกละเลย: โปรแกรมยังรับ argument ไม่ได้
- ขนาด source รวมที่ฝังได้สูงสุดราว 256 KiB
- บน macOS executable ถูกเซ็นแบบ ad-hoc ด้วย `codesign` (ต้องมี Xcode Command Line Tools)
- executable ใช้ได้กับระบบปฏิบัติการและ CPU เดียวกับ `biggo` ที่ใช้สร้าง (ไม่ cross-compile)

## biggo parse

```sh
biggo parse report.bgo
```

พิมพ์ syntax tree ของโปรแกรมในรูป S-expression ใช้ตรวจว่า parser อ่านโค้ดอย่างไร
(ลำดับของตัวดำเนินการ, จุดจบของ statement) เช่น `let total = [1, 2] |> map(fn(n) { n * 2 })` ได้

```text
(let total (|> (list 1 2) (call map (lambda (n) (block (* n 2))))))
```

## language server และ editor

`biggo lsp` คือ language server ที่พูด Language Server Protocol ผ่าน standard input/output
editor ใดก็ตามที่รองรับ LSP ใช้ได้ ความสามารถ:

| ความสามารถ | รายละเอียด |
| --- | --- |
| diagnostics | syntax และ type error ขณะพิมพ์ จากตัวตรวจเดียวกับ `biggo check` |
| hover | type ของ expression ใต้ cursor — สำหรับตารางคือรายชื่อ column ณ จุดนั้นของ pipeline |
| formatting | จัดรูปแบบทั้งไฟล์ด้วยตัวเดียวกับ `biggo fmt` |

ไฟล์ที่ `import` ถูกอ่านจากเนื้อหาที่เปิดค้างใน editor ก่อน (แม้ยังไม่ save) ถ้าไม่ได้เปิดจึงอ่านจากดิสก์
error ในไฟล์ที่ถูก import แสดงที่บรรทัด `import` ของไฟล์ที่กำลังแก้

ยังไม่มี: autocomplete, go to definition, rename

### VS Code

extension อยู่ในโฟลเดอร์ [`editors/vscode`](../editors/vscode) ให้ syntax highlighting,
การจับคู่วงเล็บ, ทุกความสามารถของ language server ข้างบน และคำสั่ง
**biggo: Run File**, **biggo: Explain Query Plans of File**, **biggo: Run Tests**

```sh
cd editors/vscode
npm install
npx vsce package --allow-missing-repository --skip-license
code --install-extension biggo-0.1.0.vsix
```

extension เรียก `biggo` จาก `PATH` หรือจาก path ที่ตั้งใน setting `biggo.path`

### editor อื่น

Neovim 0.11 ขึ้นไป:

```lua
vim.filetype.add({ extension = { bgo = "biggo" } })
vim.lsp.config("biggo", {
  cmd = { "biggo", "lsp" },
  filetypes = { "biggo" },
  root_markers = { ".git" },
})
vim.lsp.enable("biggo")
```

Helix (`~/.config/helix/languages.toml`):

```toml
[language-server.biggo]
command = "biggo"
args = ["lsp"]

[[language]]
name = "biggo"
scope = "source.biggo"
file-types = ["bgo"]
comment-token = "//"
indent = { tab-width = 2, unit = "  " }
language-servers = ["biggo"]
```

> สถานะการทดสอบ: language server ถูกทดสอบอัตโนมัติด้วยข้อความ LSP จริง (`cargo test -p biggo-lsp`)
> และ grammar ของ VS Code ถูกทดสอบด้วย engine ตัวเดียวกับที่ VS Code ใช้ (`vscode-textmate`)
> extension ถูก package เป็น `.vsix` ได้สำเร็จ แต่ **ยังไม่ได้ลองรันใน VS Code จริง**
> และการตั้งค่าของ Neovim/Helix ข้างบนเขียนตามเอกสารของ editor นั้น ๆ ยังไม่ได้ลองรัน
