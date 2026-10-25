//! Arbitrary sequences of bind, clear, run and read calls on prepared statements, with indexes that
//! are in range, one past it, negative or huge, and values of every kind.

#![no_main]

use std::ptr;

use accessor_fuzz::*;
use libfuzzer_sys::arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
enum Op {
    Null(i32),
    Int(i32, i64),
    Double(i32, f64),
    Bool(i32, i32),
    Text(i32, Vec<u8>),
    TextNts(i32, Vec<u8>),
    Decimal(i32, Vec<u8>),
    Bignum(i32, Vec<u8>),
    Datetime(i32, i64),
    Timestamptz(i32, i64),
    Clear,
    Run,
    ParamName(i32),
    ReadAt(usize, usize),
}

#[derive(Arbitrary, Debug)]
struct Input {
    /// Which statement: all of them take eight parameters (a to h), so the same ops fit each.
    which: u8,
    ops: Vec<Op>,
}

/// Mostly valid indexes, sometimes anything at all.
fn index(i: i32) -> i32 {
    if i % 5 == 0 {
        i
    } else {
        i.rem_euclid(9) - 0
    }
}

const STATEMENTS: [&std::ffi::CStr; 3] = [
    c"INSERT INTO t (i, d, s, b, dec, big, dt, tz) VALUES (@a, @b, @c, @d, @e, @f, @g, @h) RETURNING *",
    c"SELECT i, d, s FROM t WHERE i = @a OR d = @b OR s = @c OR b = @d OR dec = @e OR big = @f OR dt = @g OR tz = @h",
    c"SELECT @a, @b, @c, @d, @e, @f, @g, @h",
];

fuzz_target!(|input: Input| {
    if input.ops.len() > 64 || input.ops.iter().any(|o| matches!(o, Op::Text(_, t) | Op::TextNts(_, t) | Op::Decimal(_, t) | Op::Bignum(_, t) if t.len() > 512)) {
        return;
    }
    with_conn(|db| unsafe {
        let mut stmt = ptr::null_mut();
        assert_eq!(acc_prepare(db, STATEMENTS[input.which as usize % 3].as_ptr(), ACC_NTS, &mut stmt), ACC_OK);
        assert_eq!(acc_stmt_param_count(stmt), 8);
        let mut last: *mut acc_result = ptr::null_mut();
        for op in &input.ops {
            let (code, bound_index) = match op {
                Op::Null(i) => (acc_bind_null(stmt, index(*i)), Some(*i)),
                Op::Int(i, v) => (acc_bind_int64(stmt, index(*i), *v), Some(*i)),
                Op::Double(i, v) => (acc_bind_double(stmt, index(*i), *v), Some(*i)),
                Op::Bool(i, v) => (acc_bind_bool(stmt, index(*i), *v), Some(*i)),
                Op::Text(i, t) => (acc_bind_text(stmt, index(*i), as_ptr(t), t.len()), Some(*i)),
                Op::TextNts(i, t) => {
                    let c = c_text(t);
                    (acc_bind_text(stmt, index(*i), c.as_ptr(), ACC_NTS), Some(*i))
                }
                Op::Decimal(i, t) => (acc_bind_decimal(stmt, index(*i), as_ptr(t), t.len()), Some(*i)),
                Op::Bignum(i, t) => (acc_bind_bignum(stmt, index(*i), as_ptr(t), t.len()), Some(*i)),
                Op::Datetime(i, v) => (acc_bind_datetime(stmt, index(*i), *v), Some(*i)),
                Op::Timestamptz(i, v) => (acc_bind_timestamptz(stmt, index(*i), *v), Some(*i)),
                Op::Clear => (acc_stmt_clear_bindings(stmt), None),
                Op::Run => {
                    acc_result_free(last);
                    last = ptr::null_mut();
                    let code = acc_stmt_run(stmt, &mut last);
                    if code == ACC_OK {
                        read_all(last);
                    } else {
                        assert!(last.is_null());
                    }
                    (code, None)
                }
                Op::ParamName(i) => {
                    let name = acc_stmt_param_name(stmt, *i);
                    assert_eq!(name.is_null(), !(0..8).contains(i));
                    (ACC_OK, None)
                }
                Op::ReadAt(r, c) => {
                    if !last.is_null() {
                        let _ = acc_value_type(last, *r, *c);
                        let _ = acc_value_int64(last, *r, *c);
                        let mut len = 0;
                        let _ = acc_value_text(last, *r, *c, &mut len);
                    }
                    (ACC_OK, None)
                }
            };
            ok_code(db, code, &format!("{op:?}"));
            // An index outside the statement is a misuse, never a crash and never accepted.
            if let Some(i) = bound_index {
                if !(0..8).contains(&index(i)) {
                    assert_eq!(code, ACC_ERR_MISUSE, "index {} must be refused", index(i));
                }
            }
        }
        acc_result_free(last);
        acc_stmt_free(stmt);
    });
});
