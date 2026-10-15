//! Small deterministic functions keep timezone and Unicode matching in Rust.
use libsqlite3_sys as ffi;
use std::{
    ffi::c_void,
    panic::{catch_unwind, AssertUnwindSafe},
};

unsafe fn text(value: *mut ffi::sqlite3_value) -> Option<String> {
    if ffi::sqlite3_value_type(value) == ffi::SQLITE_NULL {
        return None;
    }
    let data = ffi::sqlite3_value_text(value);
    if data.is_null() {
        return None;
    }
    let len = ffi::sqlite3_value_bytes(value) as usize;
    Some(String::from_utf8_lossy(std::slice::from_raw_parts(data, len)).into_owned())
}

unsafe extern "C" fn scalar(ctx: *mut ffi::sqlite3_context, argc: i32, argv: *mut *mut ffi::sqlite3_value) {
    // No panic may cross the C ABI; SQLite owns the arguments for this callback.
    let result = catch_unwind(AssertUnwindSafe(|| {
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        let kind = unsafe { ffi::sqlite3_user_data(ctx) as usize };
        let output = if kind == 1 || kind == 5 {
            if unsafe { ffi::sqlite3_value_type(args[0]) } == ffi::SQLITE_NULL {
                return None;
            }
            let at = chrono::DateTime::from_timestamp_micros(unsafe { ffi::sqlite3_value_int64(args[0]) })?;
            let timezone = if kind == 1 { chrono_tz::Asia::Shanghai } else { crate::config::timezone() };
            at.with_timezone(&timezone).format("%Y-%m-%d").to_string()
        } else if kind == 2 {
            unsafe { text(args[0]) }?.to_lowercase()
        } else {
            let value = unsafe { text(args[0]) }?;
            let pattern = unsafe { text(args[1]) }?;
            let matched = like_matches(&value, &pattern, kind == 3);
            unsafe {
                ffi::sqlite3_result_int(ctx, i32::from(matched));
            }
            return Some(());
        };
        unsafe {
            ffi::sqlite3_result_text(ctx, output.as_ptr().cast(), output.len() as i32, ffi::SQLITE_TRANSIENT());
        }
        Some(())
    }));
    match result {
        Ok(Some(())) => (),
        Ok(None) => ffi::sqlite3_result_null(ctx),
        Err(_) => ffi::sqlite3_result_error(ctx, c"SQLite function failed".as_ptr(), -1),
    }
}

fn like_matches(value: &str, pattern: &str, insensitive: bool) -> bool {
    // SQL wildcards and backslash escaping, with Unicode case handling.
    let mut expression = String::from("\\A");
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '%' => expression.push_str(".*"),
            '_' => expression.push('.'),
            '\\' => match chars.next() {
                Some(c) => expression.push_str(&regex::escape(&c.to_string())),
                None => return false,
            },
            c => expression.push_str(&regex::escape(&c.to_string())),
        }
    }
    expression.push_str("\\z");
    regex::RegexBuilder::new(&expression)
        .case_insensitive(insensitive)
        .dot_matches_new_line(true)
        .build()
        .is_ok_and(|regex| regex.is_match(value))
}

pub async fn install(connection: &mut sqlx::SqliteConnection) -> Result<(), sqlx::Error> {
    let mut handle = connection.lock_handle().await?;
    for (name, argc, kind) in
        [(c"firefly_shanghai_date", 1, 1usize), (c"firefly_local_date", 1, 5), (c"firefly_lower", 1, 2), (c"firefly_ilike", 2, 3), (c"firefly_like", 2, 4)]
    {
        // lock_handle excludes the SQLx worker while registering callbacks.
        let code = unsafe {
            ffi::sqlite3_create_function_v2(
                handle.as_raw_handle().as_ptr(),
                name.as_ptr(),
                argc,
                ffi::SQLITE_UTF8 | ffi::SQLITE_DETERMINISTIC | ffi::SQLITE_INNOCUOUS,
                kind as *mut c_void,
                Some(scalar),
                None,
                None,
                None,
            )
        };
        if code != ffi::SQLITE_OK {
            return Err(sqlx::Error::Protocol(format!("SQLite function registration failed ({code})")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wildcard_and_unicode_semantics() {
        assert!(like_matches("/ÄBC/汉字", "%äbc/_字", true));
        assert!(!like_matches("ABC", "abc", false));
        assert!(like_matches("100%_done", r"100\%\_done", false));
        assert!(like_matches("a\nb", "a_b", false));
    }
}
