use std::collections::HashSet;
use std::io::Write;

use chumsky::container::Container;

use crate::ast::{self, Item};
use crate::error::TMDLError;

const HEADER: &'static str = "Definition word := Z.

Inductive binop := BAdd | BSub | BAnd | BOr | BXor | BSll | BSrl | BSra | BSlt | BSlta.
Inductive expr := EReg Z | EImm Z | EBin binop expr expr.
Record Stmt := { dst: Z; body: expr }.

(* Width and truncation utilities *)
Definition pow2 (w:Z) := 2 ^ w.
Definition maskw (w:Z) := pow2 w - 1.
Definition trunc (w z:Z) := Z.land z (maskw w).
Definition shamt_mask (w z:Z) := Z.land z (w - 1).  (* for shifts *)

(* FIXME this is hardcoded for now, need to emit in a more robust way *)
Module Params.
  Parameter XLEN : Z.
  Axiom XLEN_pos : 0 < XLEN.
End Params.

Record State := { pc : Z; rf : Z -> Z }.

Definition read_reg (s:State) (i:Z) : Z :=
  if Z.eqb i 0 then 0 else s.(rf) i.

Definition write_reg (s:State) (i v:Z) : State :=
  if Z.eqb i 0 then s
  else {| pc := s.(pc);
          rf := fun j => if Z.eqb j i then v else s.(rf) j |}.

";

const INST_DESC: &'static str = "(* One instruction descriptor *)
Record InstrDesc := {
  mask  : Z;                      (* encoding mask *)
  pat   : Z;                      (* encoding value *)
  match_word (w:word) : Prop := Z.land w mask = pat;

  (* extract fields from the word (fail if malformed) *)
  decode : word -> option Fields;

  (* behavior AST parameterized by decoded fields *)
  sem    : Fields -> Stmt
}.

";

const DECODE_TABLE: &'static str =
    "Fixpoint decode_table (ws: list InstrDesc) (w:word) : option (InstrDesc * Fields) :=
  match ws with
  | [] => None
  | d::ds =>
      if Z.eqb (Z.land w d.(mask)) d.(pat)
      then match d.(decode) w with
           | Some f => Some (d,f)
           | None   => decode_table ds w
           end
      else decode_table ds w
  end.

Definition step (tbl : list InstrDesc) (s:State) (iw:word) : option State :=
    match decode_table tbl iw with
    | None => None
    | Some (d,f) =>
        let s' := exec_stmt s (d.(sem) f) in
        Some {| pc := next_pc s; rf := s'.(rf) |}
    end.

";

pub fn generate_rocq(ast: Vec<ast::File>, mut output: Box<dyn Write>) -> Result<(), TMDLError> {
    output.write(HEADER.as_bytes())?;

    let mut operand_names = HashSet::new();

    for file in ast {
        for item in file.items {
            match item {
                Item::Template(template) => {
                    for (name, _) in template.operands {
                        operand_names.push(name);
                    }
                }
                Item::Instruction(instruction) => {
                    for (name, _) in instruction.operands {
                        operand_names.push(name);
                    }
                }
                _ => {}
            }
        }
    }

    let operand_names = operand_names
        .into_iter()
        .map(|name| format!("{}: Z", name))
        .collect::<Vec<_>>()
        .join("; ");

    write!(output, "Record Fields := {{ {} }}.\n\n", operand_names)?;

    output.write(INST_DESC.as_bytes())?;

    output.write(DECODE_TABLE.as_bytes())?;

    Ok(())
}
