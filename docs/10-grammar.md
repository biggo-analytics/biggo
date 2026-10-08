# grammar

ไวยากรณ์ของ biggo แบบเป็นทางการ เขียนด้วย EBNF: `[ x ]` คือมีหรือไม่มีก็ได้, `{ x }` คือซ้ำกี่ครั้งก็ได้
รวมศูนย์ครั้ง, `|` คือทางเลือก, ข้อความใน `"..."` คือ token ตามตัวอักษร
คำอธิบายเชิงใช้งานอยู่ใน [ตัวภาษา](02-language.md)

## token

```text
NAME      = (ตัวอักษร | "_") { ตัวอักษร | ตัวเลข | "_" }      ไม่ใช่คำสงวน
          | "`" { อักขระใดก็ได้ที่ไม่ใช่ "`" หรือการขึ้นบรรทัดใหม่ } "`"
INT       = หลัก { หลัก | "_" }
FLOAT     = INT "." INT [ เลขชี้กำลัง ] | INT เลขชี้กำลัง
เลขชี้กำลัง = ("e" | "E") [ "+" | "-" ] หลัก { หลัก }
DECIMAL   = INT [ "." INT ] "d"
STRING    = '"' { อักขระ | escape } '"'                     อยู่ในบรรทัดเดียว
escape    = "\n" | "\t" | "\r" | "\0" | "\\" | '\"'
DATE      = "@" YYYY "-" MM "-" DD
DATETIME  = DATE "T" HH ":" MM [ ":" SS [ "." หลัก{1,6} ] ]
comment   = "//" ถึงท้ายบรรทัด
```

- "ตัวอักษร" คืออักขระ Unicode ที่ขึ้นต้นชื่อได้ (XID_Start) และ "ตัวอักษร | ตัวเลข" ที่ตามมาคือ
  XID_Continue ชื่อจึงเป็นภาษาไทยได้
- คำสงวน: `let` `fn` `type` `import` `if` `else` `match` `and` `or` `not` `true` `false` `null`
- `_` ตัวเดียวเป็นชื่อธรรมดา ยกเว้นใน pattern ของ `match` ซึ่งหมายถึง "ทุกค่า"
- ตัวเลขที่ตามด้วยตัวอักษรทันที (`5days`) เป็น error
- `DATE` และ `DATETIME` ต้องเป็นวันและเวลาที่มีจริง (`@2026-02-30` เป็น error)
- `DECIMAL` มีได้ไม่เกิน 6 หลักหลังจุด และ 32 หลักหน้าจุด

## statement

```text
program   = { statement }
statement = import | type_decl | let | fn_decl | expr

import    = "import" STRING
type_decl = "type" NAME "=" type
let       = "let" NAME [ ":" type ] "=" expr
fn_decl   = "fn" NAME "(" [ param { "," param } [ "," ] ] ")" [ "->" type ] block
param     = NAME ":" type
```

- `import` และ `type_decl` อยู่ได้เฉพาะระดับบนสุดของไฟล์ และ `import` ต้องมาก่อน statement อื่น
- `fn` ตามด้วย `NAME` คือการประกาศฟังก์ชัน; `fn` ตามด้วย `(` คือ lambda (เป็น expression)
- statement คั่นด้วยการขึ้นบรรทัดใหม่ ไม่มี `;`

### การขึ้นบรรทัดใหม่

การขึ้นบรรทัดใหม่จบ statement ยกเว้นเมื่อ

1. อยู่ภายใน `( )` หรือ `[ ]` ที่ยังไม่ปิด (รวมถึง argument ของการเรียกฟังก์ชัน)
2. token สุดท้ายของบรรทัดเป็นตัวดำเนินการแบบสองข้าง หรือ `=` `:` `->` `=>` `,`
3. token แรกของบรรทัดถัดไปเป็นตัวดำเนินการแบบสองข้างที่ขึ้นต้น expression ไม่ได้:
   `|>` `+` `*` `/` `%` `==` `!=` `<` `<=` `>` `>=` `??` `and` `or` `.`

`-` `(` `[` ที่ต้นบรรทัดเริ่ม statement ใหม่เสมอ ภายใน `{ }` ของ block และของ `match`
การขึ้นบรรทัดใหม่กลับมาคั่น statement/arm แม้ block นั้นอยู่ในวงเล็บ

## type

```text
type       = type_atom [ "?" ]
type_atom  = NAME [ "<" type { "," type } [ "," ] ">" ]
           | "{" [ field_type { "," field_type } [ "," ] ] "}"
           | "fn" "(" [ type { "," type } [ "," ] ] ")" [ "->" type ]
field_type = NAME ":" type
```

- `NAME` ของ type ที่มีในตัว: `int` `float` `bool` `string` `date` `datetime` `duration` `decimal`
  (ไม่มี argument), `list<T>` `table<T>` (หนึ่ง argument), `map<K, V>` (สอง) นอกนั้นคือ alias
  ที่ประกาศด้วย `type`
- `?` หลัง type ของฟังก์ชันเป็นของผลลัพธ์: `fn() -> int?` คือฟังก์ชันที่คืน `int?`

## expression

เรียงจากผูกหลวมที่สุดไปแน่นที่สุด:

```text
expr       = pipe
pipe       = or { "|>" call }
or         = and { "or" and }
and        = not { "and" not }
not        = "not" not | comparison
comparison = coalesce [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) coalesce ]
coalesce   = additive [ "??" coalesce ]
additive   = term { ( "+" | "-" ) term }
term       = unary { ( "*" | "/" | "%" ) unary }
unary      = "-" unary | postfix
postfix    = primary { call_args | "." NAME | "[" expr "]" }

call       = postfix ที่ลงท้ายด้วย call_args
call_args  = [ "<" type { "," type } ">" ] "(" [ arg { "," arg } [ "," ] ] ")"
arg        = [ NAME "=" ] expr
```

- ฝั่งขวาของ `|>` ต้องเป็นการเรียกฟังก์ชัน: `a |> f(b)` คือ `f(a, b)`
- การเปรียบเทียบต่อกันไม่ได้ (`a < b < c` เป็น error)
- `??` จัดกลุ่มจากขวา: `a ?? b ?? c` คือ `a ?? (b ?? c)`
- `(` และ `[` ของ `postfix` ต้องอยู่บรรทัดเดียวกับสิ่งที่มันต่อท้าย
- ใน `arg` ชื่อที่ตามด้วย `=` (ไม่ใช่ `==`) คือ argument แบบระบุชื่อ การเรียกฟังก์ชันทั่วไปต้องให้
  argument ตามตำแหน่งมาก่อน ส่วน operation ของตารางรับ `name = expr` ปนกับชื่อ column
  ในลำดับใดก็ได้

```text
primary = INT | FLOAT | DECIMAL | STRING | DATE | DATETIME
        | "true" | "false" | "null"
        | NAME
        | "(" expr ")"
        | list | record | map | block | if | match | lambda

list    = "[" [ expr { "," expr } [ "," ] ] "]"
record  = "{" NAME ":" expr { "," NAME ":" expr } [ "," ] "}"
map     = "{" key ":" expr { "," key ":" expr } [ "," ] "}"
key     = literal ที่ไม่ใช่ null                              (key ตัวแรกบอกว่าเป็น map)
block   = "{" { statement } "}"
if      = "if" expr block [ "else" ( if | block ) ]
match   = "match" expr "{" { arm } "}"
arm     = pattern { "|" pattern } "=>" expr                  arm คั่นด้วยบรรทัดใหม่หรือ ","
pattern = "_" | [ "-" ] literal
lambda  = "fn" "(" [ lparam { "," lparam } [ "," ] ] ")" [ "->" type ] block
lparam  = NAME [ ":" type ]
```

การแยก `{`:

| สิ่งที่ตามหลัง `{` | ความหมาย |
| --- | --- |
| `NAME :` | record |
| literal `:` | map |
| อย่างอื่น (รวมถึง `}`) | block |

`{}` จึงเป็น block ว่าง ซึ่งถูกตีความเป็น map ว่างเมื่ออยู่ในที่ที่ต้องการ map
ตัวของ `if` และของฟังก์ชันเป็น block เสมอ

## ข้อจำกัดของ parser

- การซ้อน (วงเล็บ, block, type) ลึกได้ 256 ชั้น
- เมื่อพบ syntax error parser ข้ามไปเริ่มใหม่ที่ statement ถัดไป จึงรายงานได้หลาย error ในรอบเดียว

## ตัวอย่างที่ parser มองเห็น

`biggo parse file.bgo` พิมพ์ syntax tree ในรูป S-expression:

```text
$ cat demo.bgo
let total = 1 + 2 * 3
sales |> where(qty > 0 and not is_null(price)) |> take(5)
let f = fn(x: int) -> int { -x ?? 0 }

$ biggo parse demo.bgo
(let total (+ 1 (* 2 3)))
(|> sales (call where (and (> qty 0) (not (call is_null price)))) (call take 5))
(let f (lambda (x: int) -> int (block (?? (- x) 0))))
```
