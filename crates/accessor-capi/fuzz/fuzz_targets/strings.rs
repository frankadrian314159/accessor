//! Feeds arbitrary bytes to every function of the C interface that takes text: SQL, report names,
//! parameter names and bound text, in both the (pointer, length) and the NUL-terminated forms, and
//! with lengths that cut multi-byte characters in half.

#![no_main]

use std::ptr;

use accessor_fuzz::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 || data.len() > 4096 {
        return;
    }
    let (op, rest) = (data[0] % 10, &data[1..]);
    if matches!(op, 0..=2 | 8) && too_expensive(rest) {
        return;
    }
    with_conn(|db| unsafe {
        match op {
            // SQL given with its length: any bytes at all, valid UTF-8 or not.
            0 => {
                let mut res = ptr::null_mut();
                let code = acc_exec(db, as_ptr(rest), rest.len(), &mut res);
                ok_code(db, code, "acc_exec");
                if code == ACC_OK {
                    read_all(res);
                } else {
                    assert!(res.is_null());
                }
                acc_result_free(res);
            }
            // The same, NUL-terminated.
            1 => {
                let text = c_text(rest);
                let mut res = ptr::null_mut();
                let code = acc_exec(db, text.as_ptr(), ACC_NTS, &mut res);
                ok_code(db, code, "acc_exec (NUL-terminated)");
                if code == ACC_OK {
                    read_all(res);
                }
                acc_result_free(res);
            }
            // Read once, run with everything NULL.
            2 => {
                let mut stmt = ptr::null_mut();
                let code = acc_prepare(db, as_ptr(rest), rest.len(), &mut stmt);
                ok_code(db, code, "acc_prepare");
                if code == ACC_OK {
                    assert!(!stmt.is_null());
                    let n = acc_stmt_param_count(stmt);
                    assert!(n >= 0);
                    for i in 0..n {
                        let name = acc_stmt_param_name(stmt, i);
                        assert!(!name.is_null());
                        assert_eq!(acc_stmt_param_index(stmt, name, ACC_NTS), i, "a parameter is found by its own name");
                        ok_code(db, acc_bind_null(stmt, i), "acc_bind_null");
                    }
                    let mut res = ptr::null_mut();
                    let code = acc_stmt_run(stmt, &mut res);
                    ok_code(db, code, "acc_stmt_run");
                    if code == ACC_OK {
                        read_all(res);
                    }
                    acc_result_free(res);
                    acc_stmt_free(stmt);
                } else {
                    assert!(stmt.is_null());
                }
            }
            // Report names and formats.
            3 => {
                let (fmt, name) = (rest[0] as i32 % 7 - 1, &rest[1..]);
                let (mut buf, mut len) = (ptr::null_mut(), 0usize);
                let code = acc_render_report(db, as_ptr(name), name.len(), fmt, &mut buf, &mut len);
                ok_code(db, code, "acc_render_report");
                if code == ACC_OK {
                    assert!(!buf.is_null());
                    let bytes = std::slice::from_raw_parts(buf, len);
                    if fmt == ACC_REPORT_PDF {
                        assert!(bytes.starts_with(b"%PDF"));
                    } else {
                        assert!(std::str::from_utf8(bytes).is_ok(), "a text report is not UTF-8");
                    }
                    acc_buffer_free(buf, len);
                } else {
                    assert!(buf.is_null() && len == 0);
                }
            }
            // Parameter lookup by an arbitrary name.
            4 => {
                let mut stmt = ptr::null_mut();
                assert_eq!(acc_prepare(db, c"SELECT @alpha, @Beta, @gamma_3".as_ptr(), ACC_NTS, &mut stmt), ACC_OK);
                let i = acc_stmt_param_index(stmt, as_ptr(rest), rest.len());
                assert!((-1..3).contains(&i));
                if i >= 0 {
                    let name = std::str::from_utf8(rest).expect("a name that matched is text").trim_start_matches('@');
                    assert!(["alpha", "beta", "gamma_3"][i as usize].eq_ignore_ascii_case(name));
                }
                acc_stmt_free(stmt);
            }
            // Bound text of every kind, into a statement that stores it.
            5 | 6 | 7 => {
                let mut stmt = ptr::null_mut();
                let sql = c"INSERT INTO t (s, dec, big) VALUES (@s, @d, @b) RETURNING s, dec, big";
                assert_eq!(acc_prepare(db, sql.as_ptr(), ACC_NTS, &mut stmt), ACC_OK);
                let nts = c_text(rest);
                let (ptr_, len) = if op == 7 { (nts.as_ptr(), ACC_NTS) } else { (as_ptr(rest), rest.len()) };
                ok_code(db, acc_bind_text(stmt, 0, ptr_, len), "acc_bind_text");
                ok_code(db, acc_bind_decimal(stmt, 1, ptr_, len), "acc_bind_decimal");
                ok_code(db, acc_bind_bignum(stmt, 2, ptr_, len), "acc_bind_bignum");
                for i in 0..3 {
                    // whatever was refused, the other parameters need values to run
                    ok_code(db, acc_bind_null(stmt, i), "acc_bind_null");
                }
                ok_code(db, acc_bind_text(stmt, 0, ptr_, len), "acc_bind_text");
                let mut res = ptr::null_mut();
                let code = acc_stmt_run(stmt, &mut res);
                ok_code(db, code, "acc_stmt_run");
                if code == ACC_OK {
                    read_all(res);
                    // what was stored comes back byte for byte
                    if std::str::from_utf8(rest).is_ok() && op != 7 {
                        let mut got = 0usize;
                        let text = acc_value_text(res, 0, 0, &mut got);
                        assert!(!text.is_null() && std::slice::from_raw_parts(text.cast::<u8>(), got) == rest, "bound text changed on the way");
                    }
                }
                acc_result_free(res);
                acc_stmt_free(stmt);
            }
            // SQL cut at an arbitrary byte: UTF-8 characters and tokens split in two.
            8 => {
                let cut = rest[0] as usize % rest.len();
                let sql = &rest[1..];
                let sql = &sql[..cut.min(sql.len())];
                let code = acc_exec(db, as_ptr(sql), sql.len(), ptr::null_mut());
                ok_code(db, code, "acc_exec (cut)");
            }
            // Open: paths that are not valid text, or are directories that cannot be made.
            _ => {
                let mut out = ptr::null_mut();
                let name = c_text(rest);
                // Only memory databases: no files are made from fuzz data.
                let path = if rest.first() == Some(&b':') { c":memory:" } else { name.as_c_str() };
                // (checked on the path as the library sees it: cut at the first NUL)
                if path == c":memory:" || std::str::from_utf8(name.to_bytes()).is_err() {
                    let code = acc_open(path.as_ptr(), &mut out);
                    assert!(!out.is_null());
                    assert!(code == ACC_OK || code == ACC_ERR_ENCODING || code == ACC_ERR_IO, "open returned {code}");
                    acc_close(out);
                }
            }
        }
    });
});
