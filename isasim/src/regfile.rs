use std::{cell::RefCell, rc::Rc};
use winnow::Parser;

const MAX_REG_SIZE: usize = 256;

#[derive(Debug, Clone)]
pub struct Value {
    data: [u8; MAX_REG_SIZE],
}

macro_rules! value_from_impl {
    ($ty:ty, $vty:ty) => {
        impl From<$ty> for Value {
            fn from(value: $ty) -> Self {
                let mut data: [u8; MAX_REG_SIZE] = [0; MAX_REG_SIZE];
                let value_bytes = value.to_be_bytes();
                let offset = MAX_REG_SIZE - std::mem::size_of::<$vty>();

                for i in 0..std::mem::size_of::<$vty>() {
                    data[offset + i] = value_bytes[i];
                }

                Self { data }
            }
        }
    };
}

macro_rules! value_from {
    ($ty:ty) => {
        value_from_impl!($ty, $ty);
        value_from_impl!(&$ty, $ty);
    };
}

impl Value {
    pub fn get_lower(&self) -> u32 {
        u32::from_be_bytes(
            self.data[MAX_REG_SIZE - 4..MAX_REG_SIZE]
                .try_into()
                .unwrap(),
        )
    }
}

impl Default for Value {
    fn default() -> Self {
        let data = [0; MAX_REG_SIZE];
        Self { data }
    }
}

value_from!(u64);
value_from!(u32);
value_from!(u16);
value_from!(u8);
value_from!(i64);
value_from!(i32);
value_from!(i16);
value_from!(i8);

pub trait RegFile {
    fn read_register(&self, reg_name: &str) -> Value;
    fn write_register(&mut self, reg_name: &str, value: &Value);
    fn dump(&self) -> String;
}

#[derive(Debug)]
pub struct RISCVRegFile {
    registers: Vec<Value>,
}

impl RISCVRegFile {
    pub fn new() -> Rc<RefCell<Self>> {
        let mut registers = vec![];
        registers.resize(32, Value::default());

        Rc::new(RefCell::new(Self { registers }))
    }
}

impl RegFile for RISCVRegFile {
    fn read_register(&self, reg_name: &str) -> Value {
        let reg = tir_riscv::register_parser.parse(reg_name).unwrap();
        self.registers[tir_riscv::get_reg_num(&reg)].clone()
    }

    fn write_register(&mut self, reg_name: &str, value: &Value) {
        let reg = tir_riscv::register_parser.parse(reg_name).unwrap();

        // hardwired zero
        if let tir_riscv::Register::X0 = reg {
            return;
        }

        self.registers[tir_riscv::get_reg_num(&reg)] = value.clone();
    }

    fn dump(&self) -> String {
        let mut strings = vec![];
        strings.push("{".to_string());

        for id in 0..self.registers.len() {
            let reg: tir_riscv::Register = TryFrom::try_from(id).expect("A valid register");
            strings.push(format!(
                "    \"{}\": {},",
                tir_riscv::get_reg_name(&reg),
                self.registers[id].get_lower()
            ));
        }

        strings.push("}".to_string());

        strings.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use crate::{RISCVRegFile, RegFile};

    #[test]
    fn riscv_regfile() {
        let reg_file: Rc<RefCell<dyn RegFile>> = RISCVRegFile::new();

        let value = 42;
        reg_file.borrow_mut().write_register("x1", &value.into());
        let other_value = reg_file.borrow().read_register("x1").get_lower();

        assert_eq!(value, other_value);
    }
}
