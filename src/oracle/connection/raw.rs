use oci_sys as ffi;
use std::ffi::CString;
use std::os::raw as libc;
use std::ptr;
use std::str;

use diesel::result::*;

use super::stmt::Statement;

pub struct ConnectionEnviroment {
    handle: *mut ffi::OCIEnv,
    pub error_handle: *mut ffi::OCIError,
    pub cs_id: u16,
}

impl ConnectionEnviroment {
    pub fn new() -> Result<ConnectionEnviroment, ConnectionError> {
        let env_handle = unsafe {
            let mut handle: *mut ffi::OCIEnv = ptr::null_mut();
            let code = ffi::OCIEnvNlsCreate(
                &mut handle as *mut _,
                ffi::OCI_DEFAULT,
                ptr::null_mut(),
                None,
                None,
                None,
                0,
                ptr::null_mut(),
                0,
                0,
            );
            if code != 0 {
                return Err(ConnectionError::BadConnection(format!(
                    "Couldn't create Environment: {:?}",
                    code
                )));
            }
            handle
        };
        let error_handle =
            unsafe { alloc_handle::<ffi::OCIError>(env_handle, ffi::OCI_HTYPE_ERROR) };
        // we are certain that our string doesn't have 0 bytes in the middle,
        // so we can .unwrap()
        let enc = CString::new("UTF8").unwrap();
        let cs_id = unsafe {
            ffi::OCINlsCharSetNameToId(
                env_handle as *mut libc::c_void,
                enc.as_ptr() as *const ffi::OraText,
            )
        };
        Ok(ConnectionEnviroment {
            handle: env_handle,
            error_handle,
            cs_id,
        })
    }
}

impl Drop for ConnectionEnviroment {
    fn drop(&mut self) {
        unsafe {
            ffi::OCIHandleFree(self.error_handle as *mut libc::c_void, ffi::OCI_HTYPE_ERROR);
            ffi::OCIHandleFree(self.handle as *mut libc::c_void, ffi::OCI_HTYPE_ENV);
        }
    }
}

pub struct RawConnection {
    pub env: ConnectionEnviroment,
    pub service_handle: *mut ffi::OCISvcCtx,
    server_handle: *mut ffi::OCIServer,
    session_handle: *mut ffi::OCISession,
    transaction_handle: *mut ffi::OCITrans,
}

unsafe fn alloc_handle<R>(env: *mut ffi::OCIEnv, tpe: libc::c_uint) -> *mut R {
    let mut handle = ptr::null_mut();
    ffi::OCIHandleAlloc(
        env as *const _,
        (&mut handle as *mut *mut R) as *mut _,
        tpe,
        0,
        ptr::null_mut(),
    );
    handle
}

fn parse_db_string(database_url: &str) -> ConnectionResult<(String, String, String)> {
    if !database_url.starts_with("oci://") {
        let msg = format!("Could not use {} with oci backend", database_url);
        return Err(ConnectionError::InvalidConnectionUrl(msg));
    }

    // example: oci://\"diesel\"/diesel@//192.168.2.81:1521/orcl, c.f. sqplus manual

    let splits: Vec<&str> = database_url.split("//").collect();
    assert_eq!(splits.len(), 3);
    let userandpw: Vec<&str> = splits[1].split('/').collect();
    let user = userandpw[0].to_string();
    let mut password = userandpw[1].to_string();
    password.pop();
    let db_url = splits[2].to_string();

    Ok((user, password, db_url))
}

impl RawConnection {
    pub fn establish(database_url: &str) -> ConnectionResult<Self> {
        let (username, password, database) = parse_db_string(database_url)?;

        // Initialize environment
        let env = ConnectionEnviroment::new()?;

        unsafe {
            // Allocate the server handle
            let server_handle =
                alloc_handle(env.handle, ffi::OCI_HTYPE_SERVER);
            // Allocate the service context handle
            let service_handle =
                alloc_handle(env.handle, ffi::OCI_HTYPE_SVCCTX);

            // Allocate the session handle
            let session_handle =
                alloc_handle(env.handle, ffi::OCI_HTYPE_SESSION);

            let transaction_handle =
                alloc_handle(env.handle, ffi::OCI_HTYPE_TRANS);

            let status = ffi::OCIServerAttach(
                server_handle,
                env.error_handle,
                (&database).as_ptr() as *const libc::c_uchar,
                database.len() as i32,
                ffi::OCI_DEFAULT,
            );

            Statement::check_error(env.error_handle, status)
                .map_err(|e| ConnectionError::BadConnection(format!("{:?}", e)))?;

            // Set attribute server context in the service context
            ffi::OCIAttrSet(
                service_handle as *mut libc::c_void,
                ffi::OCI_HTYPE_SVCCTX,
                server_handle as *mut libc::c_void,
                0,
                ffi::OCI_ATTR_SERVER,
                env.error_handle,
            );
            // Set attribute username in the session context
            ffi::OCIAttrSet(
                session_handle as *mut libc::c_void,
                ffi::OCI_HTYPE_SESSION,
                username.as_ptr() as *mut libc::c_void,
                username.len() as u32,
                ffi::OCI_ATTR_USERNAME,
                env.error_handle,
            );
            // Set attribute password in the session context
            ffi::OCIAttrSet(
                session_handle as *mut libc::c_void,
                ffi::OCI_HTYPE_SESSION,
                password.as_ptr() as *mut libc::c_void,
                password.len() as u32,
                ffi::OCI_ATTR_PASSWORD,
                env.error_handle,
            );
            // Begin session
            let status = ffi::OCISessionBegin(
                service_handle,
                env.error_handle,
                session_handle,
                ffi::OCI_CRED_RDBMS,
                ffi::OCI_DEFAULT,
            );
            Statement::check_error(env.error_handle, status)
                .map_err(|e| ConnectionError::BadConnection(format!("{:?}", e)))?;

            // Set session context in the service context
            ffi::OCIAttrSet(
                service_handle as *mut libc::c_void,
                ffi::OCI_HTYPE_SVCCTX,
                session_handle as *mut libc::c_void,
                0,
                ffi::OCI_ATTR_SESSION,
                env.error_handle,
            );

            ffi::OCIAttrSet(
                service_handle as *mut libc::c_void,
                ffi::OCI_HTYPE_SVCCTX,
                transaction_handle as *mut libc::c_void,
                0,
                ffi::OCI_ATTR_TRANS,
                env.error_handle,
            );

            Ok(RawConnection {
                env,
                service_handle,
                server_handle,
                session_handle,
                transaction_handle,
            })
        }
    }
}

impl Drop for RawConnection {
    fn drop(&mut self) {
        unsafe {
            ffi::OCISessionEnd(
                self.service_handle,
                self.env.error_handle,
                self.session_handle,
                ffi::OCI_DEFAULT,
            );
            ffi::OCIServerDetach(self.server_handle, self.env.error_handle, ffi::OCI_DEFAULT);
            ffi::OCIHandleFree(
                self.session_handle as *mut libc::c_void,
                ffi::OCI_HTYPE_SESSION,
            );
            ffi::OCIHandleFree(
                self.service_handle as *mut libc::c_void,
                ffi::OCI_HTYPE_SVCCTX,
            );
            ffi::OCIHandleFree(self.server_handle as *mut libc::c_void, ffi::OCI_HTYPE_ENV);
            ffi::OCIHandleFree(
                self.transaction_handle as *mut libc::c_void,
                ffi::OCI_HTYPE_TRANS,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_db_string;

    #[test]
    fn check_parse_database_url_1() {
        let input = "oci://user/password@//localhost:1234/my_database";
        let output = parse_db_string(input).unwrap();
        assert_eq!(
            output,
            (
                "user".into(),
                "password".into(),
                "localhost:1234/my_database".into()
            )
        );
    }

    #[test]
    fn check_parse_database_url_2() {
        let input = "oci://user/password@//localhost/my_database";
        let output = parse_db_string(input).unwrap();
        assert_eq!(
            output,
            (
                "user".into(),
                "password".into(),
                "localhost/my_database".into()
            )
        );
    }
}
