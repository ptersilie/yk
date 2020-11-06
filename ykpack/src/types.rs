//! Types for the Yorick intermediate language.

use serde::{Deserialize, Serialize};
use std::{
    convert::TryFrom,
    fmt::{self, Display},
    mem,
};

// FIXME these should probably all be tuple structs, as type aliases offer little type safety.
pub type DefIndex = u32;
pub type BasicBlockIndex = u32;
pub type StatementIndex = usize;
pub type LocalIndex = u32;
pub type TyIndex = u32;
pub type FieldIndex = u32;
pub type ArrayIndex = u32;
pub type TypeId = (CguHash, TyIndex); // CGU hash and vector index.
pub type OffT = i32;

#[derive(Debug, Hash, Eq, PartialEq, Copy, Clone, Serialize, Deserialize)]
pub struct CguHash(pub u64);

impl Display for CguHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

/// The type of a local variable.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub enum Ty {
    /// Signed integers.
    SignedInt(SignedIntTy),
    /// Unsigned integers.
    UnsignedInt(UnsignedIntTy),
    /// A structure type.
    Struct(StructTy),
    /// A tuple type.
    Tuple(TupleTy),
    /// An array type.
    /// FIXME size_align can be computed from elem_ty and len, but requires some refactoring.
    Array {
        elem_ty: TypeId,
        len: usize,
        size_align: SizeAndAlign,
    },
    /// A slice type.
    Slice(TypeId),
    /// A reference to something.
    Ref(TypeId),
    /// A Boolean.
    Bool,
    /// A char.
    Char,
    /// Anything that we've not yet defined a lowering for.
    Unimplemented(String),
}

impl Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::SignedInt(si) => write!(f, "{}", si),
            Ty::UnsignedInt(ui) => write!(f, "{}", ui),
            Ty::Struct(sty) => write!(f, "{}", sty),
            Ty::Tuple(tty) => write!(f, "{}", tty),
            Ty::Array { elem_ty, len, .. } => {
                write!(f, "[({}, {}); {}]", elem_ty.0, elem_ty.1, len)
            }
            Ty::Slice(sty) => write!(f, "&[{:?}]", sty),
            Ty::Ref(rty) => write!(f, "&{:?}", rty),
            Ty::Bool => write!(f, "bool"),
            Ty::Char => write!(f, "char"),
            Ty::Unimplemented(m) => write!(f, "Unimplemented: {}", m),
        }
    }
}

impl Ty {
    pub fn size(&self) -> u64 {
        match self {
            Ty::UnsignedInt(ui) => match ui {
                UnsignedIntTy::U8 => 1,
                UnsignedIntTy::U16 => 2,
                UnsignedIntTy::U32 => 4,
                UnsignedIntTy::U64 => 8,
                UnsignedIntTy::Usize => u64::try_from(mem::size_of::<usize>()).unwrap(),
                UnsignedIntTy::U128 => 16,
            },
            Ty::SignedInt(ui) => match ui {
                SignedIntTy::I8 => 1,
                SignedIntTy::I16 => 2,
                SignedIntTy::I32 => 4,
                SignedIntTy::I64 => 8,
                SignedIntTy::Isize => u64::try_from(mem::size_of::<isize>()).unwrap(),
                SignedIntTy::I128 => 16,
            },
            Ty::Struct(sty) => u64::try_from(sty.size_align.size).unwrap(),
            Ty::Tuple(tty) => u64::try_from(tty.size_align.size).unwrap(),
            Ty::Ref(_) => u64::try_from(mem::size_of::<usize>()).unwrap(),
            Ty::Bool => u64::try_from(mem::size_of::<bool>()).unwrap(),
            Ty::Char => u64::try_from(mem::size_of::<char>()).unwrap(),
            Ty::Array {
                size_align: SizeAndAlign { size, .. },
                ..
            } => u64::try_from(*size).unwrap(),
            _ => todo!(),
        }
    }

    pub fn align(&self) -> u64 {
        match self {
            Ty::UnsignedInt(ui) => match ui {
                UnsignedIntTy::U8 => 1,
                UnsignedIntTy::U16 => 2,
                UnsignedIntTy::U32 => 4,
                UnsignedIntTy::U64 => 8,
                UnsignedIntTy::Usize =>
                {
                    #[cfg(target_arch = "x86_64")]
                    8
                }
                UnsignedIntTy::U128 => 16,
            },
            Ty::SignedInt(ui) => match ui {
                SignedIntTy::I8 => 1,
                SignedIntTy::I16 => 2,
                SignedIntTy::I32 => 4,
                SignedIntTy::I64 => 8,
                SignedIntTy::Isize =>
                {
                    #[cfg(target_arch = "x86_64")]
                    8
                }
                SignedIntTy::I128 => 16,
            },
            Ty::Struct(sty) => u64::try_from(sty.size_align.align).unwrap(),
            Ty::Tuple(tty) => u64::try_from(tty.size_align.align).unwrap(),
            Ty::Ref(_) =>
            {
                #[cfg(target_arch = "x86_64")]
                8
            }
            Ty::Bool => u64::try_from(mem::size_of::<bool>()).unwrap(),
            Ty::Array {
                size_align: SizeAndAlign { align, .. },
                ..
            } => u64::try_from(*align).unwrap(),
            _ => todo!("{:?}", self),
        }
    }

    pub fn is_signed_int(&self) -> bool {
        matches!(self, Self::SignedInt(..))
    }

    pub fn is_int(&self) -> bool {
        matches!(self, Self::SignedInt(..)) || matches!(self, Self::UnsignedInt(..))
    }

    pub fn unwrap_tuple(&self) -> &TupleTy {
        if let Self::Tuple(tty) = self {
            tty
        } else {
            panic!("tried to unwrap a non-tuple");
        }
    }
}

/// Describes the various signed integer types.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub enum SignedIntTy {
    Isize,
    I8,
    I16,
    I32,
    I64,
    I128,
}

impl Display for SignedIntTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Isize => "isize",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::I128 => "i128",
        };
        write!(f, "{}", s)
    }
}

/// Describes the various unsigned integer types.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub enum UnsignedIntTy {
    Usize,
    U8,
    U16,
    U32,
    U64,
    U128,
}

impl Display for UnsignedIntTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Usize => "usize",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::U128 => "u128",
        };
        write!(f, "{}", s)
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub struct Fields {
    /// Field offsets.
    pub offsets: Vec<OffT>,
    /// The type of each field.
    pub tys: Vec<TypeId>,
}

impl Display for Fields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "offsets: [{}], tys: [{}]",
            self.offsets
                .iter()
                .map(|o| o.to_string())
                .collect::<Vec<String>>()
                .join(", "),
            self.tys
                .iter()
                .map(|t| format!("({}, {})", t.0, t.1))
                .collect::<Vec<String>>()
                .join(", ")
        )
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub struct SizeAndAlign {
    /// The alignment, in bytes.
    pub align: i32, // i32 for use as a dynasm operand.
    /// The size, in bytes.
    pub size: i32, // Also i32 for dynasm.
}

impl Display for SizeAndAlign {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "align: {}, size: {}", self.align, self.size)
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub struct TupleTy {
    /// The fields of the tuple.
    pub fields: Fields,
    /// The size and alignment of the tuple.
    pub size_align: SizeAndAlign,
}

impl Display for TupleTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TupleTy {{ {}, {} }}", self.fields, self.size_align)
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub struct StructTy {
    /// The fields of the struct.
    pub fields: Fields,
    /// The size and alignment of the struct.
    pub size_align: SizeAndAlign,
}

impl Display for StructTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StructTy {{ {}, {} }}", self.fields, self.size_align)
    }
}

/// rmp-serde serialisable 128-bit numeric types, to work around:
/// https://github.com/3Hren/msgpack-rust/issues/169
macro_rules! new_ser128 {
    ($n: ident, $t: ty) => {
        #[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
        pub struct $n {
            hi: u64,
            lo: u64,
        }

        impl $n {
            pub fn new(val: $t) -> Self {
                Self {
                    hi: (val >> 64) as u64,
                    lo: val as u64,
                }
            }

            pub fn val(&self) -> $t {
                (self.hi as $t) << 64 | self.lo as $t
            }
        }

        impl Display for $n {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", self.val(), stringify!($t))
            }
        }
    };
}

new_ser128!(SerU128, u128);
new_ser128!(SerI128, i128);

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy, Hash, Ord, PartialOrd)]
pub struct Local(pub LocalIndex);

impl Display for Local {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${}", self.0)
    }
}

/// Bits in the `flags` bitfield in `Body`.
pub mod bodyflags {
    pub const DO_NOT_TRACE: u8 = 1;
    pub const INTERP_STEP: u8 = 1 << 1;
}

/// The definition of a local variable, including its type.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub struct LocalDecl {
    pub ty: TypeId,
    /// If true this local variable is at some point referenced, and thus should be allocated on
    /// the stack and never in a register.
    pub referenced: bool,
}

impl Display for LocalDecl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.ty.0, self.ty.1)
    }
}

/// A tracing IR pack.
/// Each Body maps to exactly one MIR Body.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub struct Body {
    pub symbol_name: String,
    pub blocks: Vec<BasicBlock>,
    pub flags: u8,
    pub local_decls: Vec<LocalDecl>,
    pub num_args: usize,
}

impl Display for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "symbol: {}", self.symbol_name)?;
        writeln!(f, "  flags: {}", self.flags)?;
        writeln!(f, "  num_args: {}", self.num_args)?;

        writeln!(f, "  local_decls:")?;
        for (di, d) in self.local_decls.iter().enumerate() {
            writeln!(f, "    {}: {}", di, d)?;
        }

        let mut block_strs = Vec::new();
        for (i, b) in self.blocks.iter().enumerate() {
            block_strs.push(format!("    bb{}:\n{}", i, b));
        }

        writeln!(f, "  blocks:")?;
        writeln!(f, "{}", block_strs.join("\n"))?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub struct BasicBlock {
    pub stmts: Vec<Statement>,
    pub term: Terminator,
}

impl BasicBlock {
    pub fn new(stmts: Vec<Statement>, term: Terminator) -> Self {
        Self { stmts, term }
    }
}

impl Display for BasicBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for s in self.stmts.iter() {
            writeln!(f, "        {}", s)?;
        }
        write!(f, "        {}", self.term)
    }
}

/// Represents a pointer to be dereferenced at runtime.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub struct Ptr {
    pub local: Local,
    pub off: OffT,
}

impl Display for Ptr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}+{}", self.local, self.off)
    }
}

/// An IR place. This is used in SIR and TIR to describe the (abstract) address of a piece of data.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum IPlace {
    /// The IPlace describes a value as a Local+offset pair.
    Val { local: Local, off: OffT, ty: TypeId },
    /// An indirect place behind a pointer. ykrustc uses these for deref and (dynamic) index
    /// projections (which cannot be resolved statically and thus depend on a runtime pointer).
    Indirect {
        /// The location of the pointer to be dereferenced at runtime.
        ptr: Ptr,
        /// The offset to apply to the above pointer.
        off: OffT,
        /// The type of the resulting place.
        ty: TypeId,
    },
    /// The IPlace describes a constant.
    Const { val: Constant, ty: TypeId },
    /// A construct which we have no lowering for yet.
    Unimplemented(String),
}

impl Display for IPlace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Val {
                local,
                off,
                ty: _ty,
            } => {
                if *off != 0 {
                    write!(f, "{}+{}", local, off)
                } else {
                    write!(f, "{}", local)
                }
            }
            Self::Indirect { ptr, off, ty: _ty } => {
                if *off != 0 {
                    write!(f, "*({})+{}", ptr, off)
                } else {
                    write!(f, "*{}", ptr)
                }
            }
            Self::Const { val, ty: _ty } => write!(f, "{}", val),
            Self::Unimplemented(c) => write!(f, "{}", c),
        }
    }
}

impl IPlace {
    /// Returns the local used (if any) in the place.
    pub fn local(&self) -> Option<Local> {
        match self {
            Self::Val { local, .. }
            | Self::Indirect {
                ptr: Ptr { local, .. },
                ..
            } => Some(*local),
            Self::Const { .. } | Self::Unimplemented(_) => None,
        }
    }

    /// Returns the type of the place.
    pub fn ty(&self) -> TypeId {
        match self {
            Self::Val { ty, .. } | Self::Indirect { ty, .. } | Self::Const { ty, .. } => *ty,
            Self::Unimplemented(_) => unreachable!(),
        }
    }

    /// Converts a direct place into an indirect one, forcing a dereference when read from or
    /// stored to.
    pub fn to_indirect(&self, new_ty: TypeId) -> IPlace {
        match self {
            Self::Val { local, off, ty: _ } => {
                let ptr = Ptr {
                    local: *local,
                    off: *off,
                };
                IPlace::Indirect {
                    ptr,
                    off: 0,
                    ty: new_ty,
                }
            }
            Self::Const { .. } => unreachable!("const to indirect"),
            Self::Indirect { .. } => unreachable!("indirect to indirect"),
            Self::Unimplemented(_) => self.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum Statement {
    /// Do nothing.
    Nop,
    /// Stores the content addressed by the right hand side into the left hand side.
    Store(IPlace, IPlace),
    /// Binary operations. FIXME dest should be a local?
    BinaryOp {
        dest: IPlace,
        op: BinOp,
        opnd1: IPlace,
        opnd2: IPlace,
        checked: bool,
    },
    /// Makes a reference.
    MkRef(IPlace, IPlace),
    /// Computes a pointer address at runtime.
    DynOffs {
        /// Where to store the result.
        dest: IPlace,
        /// The base address. `idx` * `scale` are added to this at runtime to give the result.
        base: IPlace,
        /// The index to multiply with `scale`.
        idx: IPlace,
        /// The scaling factor for `idx`.
        scale: u32,
    },
    /// Marks a local variable dead.
    /// Note that locals are implicitly live at first use.
    StorageDead(Local),
    /// A (non-inlined) call from a TIR trace to a binary symbol using the system ABI. This does
    /// not appear in SIR.
    Call(CallOperand, Vec<IPlace>, Option<IPlace>),
    /// Cast a value into another. Since the cast type and the destination type are the same, we
    /// only need the latter.
    Cast(IPlace, IPlace),
    /// Any unimplemented lowering maps to this variant.
    /// The string inside is the stringified MIR statement.
    Unimplemented(String),
}

impl Statement {
    fn maybe_push_local(v: &mut Vec<Local>, l: Option<Local>) {
        if let Some(l) = l {
            v.push(l);
        }
    }

    /// Returns a vector of locals that this SIR statement *may* define.
    /// Whether or not the local is actually defined depends upon whether this is the first write
    /// into the local (there is no explicit liveness marker in SIR/TIR).
    pub fn maybe_defined_locals(&self) -> Vec<Local> {
        let mut ret = Vec::new();

        match self {
            Statement::Nop => (),
            Statement::Store(dest, ..)
            | Statement::MkRef(dest, ..)
            | Statement::DynOffs { dest, .. }
            | Statement::Cast(dest, ..)
            | Statement::BinaryOp { dest, .. } => Self::maybe_push_local(&mut ret, dest.local()),
            Statement::Call(_, _, dest) => {
                if let Some(dest) = dest {
                    Self::maybe_push_local(&mut ret, dest.local());
                }
            }
            Statement::StorageDead(_) | Statement::Unimplemented(_) => (),
        }
        ret
    }

    /// Returns a vector of locals that this SIR statement uses.
    /// A definition is considered a use, so this returns a superset of what
    /// `maybe_defined_locals()` does.
    pub fn used_locals(&self) -> Vec<Local> {
        let mut ret = Vec::new();

        match self {
            Statement::Nop => (),
            Statement::MkRef(dest, src) => {
                Self::maybe_push_local(&mut ret, dest.local());
                Self::maybe_push_local(&mut ret, src.local());
            }
            Statement::DynOffs {
                dest, base, idx, ..
            } => {
                Self::maybe_push_local(&mut ret, dest.local());
                Self::maybe_push_local(&mut ret, base.local());
                Self::maybe_push_local(&mut ret, idx.local());
            }
            Statement::BinaryOp {
                dest, opnd1, opnd2, ..
            } => {
                Self::maybe_push_local(&mut ret, opnd1.local());
                Self::maybe_push_local(&mut ret, opnd2.local());
                Self::maybe_push_local(&mut ret, dest.local());
            }
            Statement::Store(dest, src) => {
                Self::maybe_push_local(&mut ret, dest.local());
                Self::maybe_push_local(&mut ret, src.local());
            }
            Statement::StorageDead(_) => (),
            Statement::Call(_target, args, dest) => {
                if let Some(dest) = dest {
                    Self::maybe_push_local(&mut ret, dest.local());
                }
                for a in args {
                    Self::maybe_push_local(&mut ret, a.local());
                }
            }
            Statement::Cast(dest, src) => {
                Self::maybe_push_local(&mut ret, dest.local());
                Self::maybe_push_local(&mut ret, src.local());
            }
            Statement::Unimplemented(_) => (),
        }
        ret
    }

    /// Returns true if the statement may affect locals besides those appearing in the statement.
    pub fn may_have_side_effects(&self) -> bool {
        matches!(self, Statement::Call(..))
    }
}

impl Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Statement::Nop => write!(f, "nop"),
            Statement::MkRef(l, r) => write!(f, "{} = &({})", l, r),
            Statement::DynOffs {
                dest,
                base,
                idx,
                scale,
                ..
            } => write!(f, "{} = dynoffs({}, {}, {})", dest, base, idx, scale),
            Statement::Store(l, r) => write!(f, "{} = {}", l, r),
            Statement::BinaryOp {
                dest,
                op,
                opnd1,
                opnd2,
                checked,
            } => {
                let c = if *checked { " (checked)" } else { "" };
                write!(f, "{} = {} {} {}{}", dest, opnd1, op, opnd2, c)
            }
            Statement::StorageDead(local) => write!(f, "dead({})", local),
            Statement::Call(op, args, dest) => {
                let args_s = args
                    .iter()
                    .map(|a| format!("{}", a))
                    .collect::<Vec<String>>()
                    .join(", ");
                let dest_s = if let Some(dest) = dest {
                    format!("{}", dest)
                } else {
                    String::from("none")
                };
                write!(f, "{} = call({}, [{}])", dest_s, op, args_s)
            }
            Statement::Cast(d, s) => write!(f, "Cast({}, {})", d, s),
            Statement::Unimplemented(mir_stmt) => write!(f, "unimplemented_stmt: {}", mir_stmt),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum Constant {
    Int(ConstantInt),
    Bool(bool),
    Tuple(TypeId), // FIXME assumed to be unit for now. Needs a value in here.
    Unimplemented(String),
}

impl Constant {
    pub fn i64_cast(&self) -> i64 {
        match self {
            Self::Int(ci) => ci.i64_cast(),
            Self::Bool(b) => *b as i64,
            Self::Tuple(..) => unreachable!(),
            Self::Unimplemented(_) => unreachable!(),
        }
    }
}

impl Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constant::Int(i) => write!(f, "{}", i),
            Constant::Bool(b) => write!(f, "{}", b),
            Constant::Tuple(..) => write!(f, "()"), // FIXME assumed unit.
            Constant::Unimplemented(s) => write!(f, "unimplemented constant: {:?}", s),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum ConstantInt {
    UnsignedInt(UnsignedInt),
    SignedInt(SignedInt),
}

impl From<bool> for ConstantInt {
    fn from(b: bool) -> Self {
        if b {
            ConstantInt::UnsignedInt(UnsignedInt::Usize(1))
        } else {
            ConstantInt::UnsignedInt(UnsignedInt::Usize(0))
        }
    }
}

impl ConstantInt {
    /// Returns an i64 value suitable for loading into a register.
    /// If the constant is signed, then it will be sign-extended.
    pub fn i64_cast(&self) -> i64 {
        match self {
            ConstantInt::UnsignedInt(ui) => match ui {
                UnsignedInt::U8(i) => *i as i64,
                UnsignedInt::U16(i) => *i as i64,
                UnsignedInt::U32(i) => *i as i64,
                UnsignedInt::U64(i) => *i as i64,
                #[cfg(target_pointer_width = "64")]
                UnsignedInt::Usize(i) => *i as i64,
                UnsignedInt::U128(_) => panic!("i64_cast: u128 to isize"),
            },
            ConstantInt::SignedInt(si) => match si {
                SignedInt::I8(i) => *i as i64,
                SignedInt::I16(i) => *i as i64,
                SignedInt::I32(i) => *i as i64,
                SignedInt::I64(i) => *i as i64,
                #[cfg(target_pointer_width = "64")]
                SignedInt::Isize(i) => *i as i64,
                SignedInt::I128(_) => panic!("i64_cast: i128 to isize"),
            },
        }
    }
}

/// Generate a method that constructs a ConstantInt variant from bits in u128 form.
/// This can't be used to generate methods for 128-bit integers due to SerU128/SerI128.
macro_rules! const_int_from_bits {
    ($fn_name: ident, $rs_t: ident, $yk_t: ident, $yk_variant: ident) => {
        pub fn $fn_name(bits: u128) -> Self {
            ConstantInt::$yk_t($yk_t::$yk_variant(bits as $rs_t))
        }
    };
}

impl ConstantInt {
    const_int_from_bits!(u8_from_bits, u8, UnsignedInt, U8);
    const_int_from_bits!(u16_from_bits, u16, UnsignedInt, U16);
    const_int_from_bits!(u32_from_bits, u32, UnsignedInt, U32);
    const_int_from_bits!(u64_from_bits, u64, UnsignedInt, U64);
    const_int_from_bits!(usize_from_bits, usize, UnsignedInt, Usize);

    pub fn u128_from_bits(bits: u128) -> Self {
        ConstantInt::UnsignedInt(UnsignedInt::U128(SerU128::new(bits)))
    }

    const_int_from_bits!(i8_from_bits, i8, SignedInt, I8);
    const_int_from_bits!(i16_from_bits, i16, SignedInt, I16);
    const_int_from_bits!(i32_from_bits, i32, SignedInt, I32);
    const_int_from_bits!(i64_from_bits, i64, SignedInt, I64);
    const_int_from_bits!(isize_from_bits, isize, SignedInt, Isize);

    pub fn i128_from_bits(bits: u128) -> Self {
        ConstantInt::SignedInt(SignedInt::I128(SerI128::new(bits as i128)))
    }
}

impl Display for ConstantInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConstantInt::UnsignedInt(u) => write!(f, "{}", u),
            ConstantInt::SignedInt(s) => write!(f, "{}", s),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum UnsignedInt {
    Usize(usize),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(SerU128),
}

impl Display for UnsignedInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usize(v) => write!(f, "{}usize", v),
            Self::U8(v) => write!(f, "{}u8", v),
            Self::U16(v) => write!(f, "{}u16", v),
            Self::U32(v) => write!(f, "{}u32", v),
            Self::U64(v) => write!(f, "{}u64", v),
            Self::U128(v) => write!(f, "{}u128", v),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum SignedInt {
    Isize(isize),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(SerI128),
}

impl Display for SignedInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Isize(v) => write!(f, "{}isize", v),
            Self::I8(v) => write!(f, "{}i8", v),
            Self::I16(v) => write!(f, "{}i16", v),
            Self::I32(v) => write!(f, "{}i32", v),
            Self::I64(v) => write!(f, "{}i64", v),
            Self::I128(v) => write!(f, "{}i128", v),
        }
    }
}

/// A call target.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum CallOperand {
    /// A call to a binary symbol by name.
    Fn(String),
    /// An unknown or unhandled callable.
    Unknown, // FIXME -- Find out what else. Closures jump to mind.
}

impl CallOperand {
    pub fn symbol(&self) -> Option<&str> {
        if let Self::Fn(sym) = self {
            Some(sym)
        } else {
            None
        }
    }
}

impl Display for CallOperand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallOperand::Fn(sym_name) => write!(f, "{}", sym_name),
            CallOperand::Unknown => write!(f, "<unknown>"),
        }
    }
}

/// A basic block terminator.
/// Note that we assume an the abort strategy, so there are no unwind or cleanup edges present.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum Terminator {
    Goto(BasicBlockIndex),
    SwitchInt {
        discr: IPlace,
        values: Vec<SerU128>,
        target_bbs: Vec<BasicBlockIndex>,
        otherwise_bb: BasicBlockIndex,
    },
    Return,
    Unreachable,
    Drop {
        location: IPlace,
        target_bb: BasicBlockIndex,
    },
    DropAndReplace {
        location: IPlace,
        target_bb: BasicBlockIndex,
        value: IPlace,
    },
    Call {
        operand: CallOperand,
        args: Vec<IPlace>,
        /// The return value and basic block to continue at, if the call converges.
        destination: Option<(IPlace, BasicBlockIndex)>,
    },
    /// The value in `cond` must equal to `expected` to advance to `target_bb`.
    Assert {
        cond: IPlace,
        expected: bool,
        target_bb: BasicBlockIndex,
    },
    Unimplemented(String), // FIXME will eventually disappear.
}

impl Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Goto(bb) => write!(f, "goto bb{}", bb),
            Terminator::SwitchInt {
                discr,
                values,
                target_bbs,
                otherwise_bb,
            } => write!(
                f,
                "switch_int {}, [{}], [{}], {}",
                discr,
                values
                    .iter()
                    .map(|b| format!("{}", b))
                    .collect::<Vec<String>>()
                    .join(", "),
                target_bbs
                    .iter()
                    .map(|b| format!("{}", b))
                    .collect::<Vec<String>>()
                    .join(", "),
                otherwise_bb
            ),
            Terminator::Return => write!(f, "return"),
            Terminator::Unreachable => write!(f, "unreachable"),
            Terminator::Drop {
                location,
                target_bb,
            } => write!(f, "drop {}, bb{}", target_bb, location,),
            Terminator::DropAndReplace {
                location,
                value,
                target_bb,
            } => write!(
                f,
                "drop_and_replace {}, {}, bb{}",
                location, value, target_bb,
            ),
            Terminator::Call {
                operand,
                args,
                destination,
            } => {
                let ret_bb = if let Some((ret_val, bb)) = destination {
                    write!(f, "{} = ", ret_val)?;
                    format!(" -> bb{}", bb)
                } else {
                    String::from("")
                };
                let args_str = args
                    .iter()
                    .map(|a| format!("{}", a))
                    .collect::<Vec<String>>()
                    .join(", ");
                write!(f, "call {}({}){}", operand, args_str, ret_bb)
            }
            Terminator::Assert {
                cond,
                target_bb,
                expected,
            } => write!(f, "assert {}, {}, bb{}", cond, target_bb, expected),
            Terminator::Unimplemented(s) => write!(f, "unimplemented: {}", s),
        }
    }
}

/// Binary operations.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitXor,
    BitAnd,
    BitOr,
    Shl,
    Shr,
    Eq,
    Lt,
    Le,
    Ne,
    Ge,
    Gt,
    Offset,
}

impl Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::BitXor => "^",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Ne => "!=",
            BinOp::Ge => ">=",
            BinOp::Gt => ">",
            BinOp::Offset => "off",
        };
        write!(f, "{}", s)
    }
}

/// The top-level pack type.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum Pack {
    Body(Body),
    Types(Types),
}

impl Display for Pack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pack::Body(sir) => write!(f, "{}", sir),
            Pack::Types(tys) => write!(f, "{:?}", tys),
        }
    }
}

/// The types used in the SIR for one specific codegen unit.
/// SIR locals reference their type using "type IDs" which are (cgu-hash, array-index) pairs.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub struct Types {
    /// A unique identifier for the codegen unit that built these types.
    pub cgu_hash: CguHash,
    /// The types themselves. Combine `cgu_hash` with an index into this to make a type ID.
    pub types: Vec<Ty>,
}

#[cfg(test)]
mod tests {
    use super::{ConstantInt, SerI128, SerU128, SignedInt, UnsignedInt};

    #[test]
    fn seru128_round_trip() {
        let val: u128 = std::u128::MAX - 427819;
        assert_eq!(SerU128::new(val).val(), val);
    }

    #[test]
    fn seri128_round_trip() {
        let val = std::i128::MIN + 77;
        assert_eq!(SerI128::new(val).val(), val);
    }

    #[test]
    fn const_u8_from_bits() {
        let v = 233;
        let cst = ConstantInt::u8_from_bits(v as u128);
        assert_eq!(cst, ConstantInt::UnsignedInt(UnsignedInt::U8(v)));
    }

    #[test]
    fn const_i32_from_bits() {
        let v = -42i32;
        let cst = ConstantInt::i32_from_bits(v as u128);
        assert_eq!(cst, ConstantInt::SignedInt(SignedInt::I32(v)));
    }

    #[test]
    fn const_u64_from_bits() {
        let v = std::u64::MAX;
        let cst = ConstantInt::u64_from_bits(v as u128);
        assert_eq!(cst, ConstantInt::UnsignedInt(UnsignedInt::U64(v)));
    }

    #[test]
    fn const_i128_from_bits() {
        let v = -100001i128;
        let cst = ConstantInt::i128_from_bits(v as u128);
        match &cst {
            ConstantInt::SignedInt(SignedInt::I128(seri128)) => assert_eq!(seri128.val(), v),
            _ => panic!(),
        }
    }
}
