//! Bindings to the networking API. It can be used to do HTTPS requests.

use crate::sys;
use std::{
    ffi::{self, CStr, CString},
    ptr, slice,
};
use std::collections::HashMap;
use std::rc::Rc;
use std::cell::RefCell;

type RegisteredCallback = Rc<RefCell<dyn Fn(String, i32)>>;

type NetworkCallback = Box<dyn FnOnce(NetworkRequest, i32)>;
// Global registry for storing instances
thread_local! {
    static CALLBACK_REGISTRY: RefCell<HashMap<i32, Rc<RefCell<dyn Fn(String, i32)>>>> = RefCell::new(HashMap::new());
}
fn id_exists(id: i32) -> bool {
    CALLBACK_REGISTRY.with(|registry| registry.borrow().contains_key(&id))
}
fn register_callback(id: i32, callback: RegisteredCallback) {
    CALLBACK_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(id, callback);
    });
}

/// A builder to build network requests
pub struct NetworkRequestBuilder<'a> {
    url: CString,
    headers: Vec<CString>,
    data: Option<&'a mut [u8]>,
    callback: Option<Box<NetworkCallback>>,
    registered_callback: Option<RegisteredCallback>,
}
impl<'a> NetworkRequestBuilder<'a> {
    /// Create a new network request
    pub fn new(url: &str) -> Option<Self> {
        Some(Self {
            url: CString::new(url).ok()?,
            headers: vec![],
            data: None,
            callback: None,
            registered_callback: None,
        })
    }
    pub fn with_registered_callback(mut self, callback: RegisteredCallback) -> Self {
        self.registered_callback = Some(callback);
        self
    }
    
    /// Set a HTTP header
    pub fn with_header(mut self, header: &str) -> Option<Self> {
        self.headers.push(CString::new(header).ok()?);
        Some(self)
    }

    /// Set the data to be sent
    pub fn with_data(mut self, data: &'a mut [u8]) -> Self {
        self.data = Some(data);
        self
    }

    /// Set a callback which will be called after the request finished or failed.
    /// The parameters are the network request and the http status code (negative if failed)
    pub fn with_callback(mut self, callback: impl FnOnce(NetworkRequest, i32) + 'static) -> Self {
        self.callback = Some(Box::new(Box::new(callback)));
        self
    }

    /// Do HTTP GET request
    pub fn get(self) -> Option<NetworkRequest> {
        self.do_request(None, sys::fsNetworkHttpRequestGet)
    }

    /// Do HTTP POST request
    pub fn post(self, post_field: &str) -> Option<NetworkRequest> {
        let post_field = CString::new(post_field).unwrap();
        self.do_request(Some(post_field), sys::fsNetworkHttpRequestPost)
    }

    /// Do HTTP PUT request
    pub fn put(self) -> Option<NetworkRequest> {
        self.do_request(None, sys::fsNetworkHttpRequestPut)
    }

    fn do_request(
        mut self,
        post_field: Option<CString>,
        request: unsafe extern "C" fn(
            *const ::std::os::raw::c_char,
            *mut sys::FsNetworkHttpRequestParam,
            sys::HttpRequestCallback,
            *mut ::std::os::raw::c_void,
        ) -> sys::FsNetworkRequestId,
    ) -> Option<NetworkRequest> {
        // SAFETY: we need a *mut i8 for the FsNetworkHttpRequestParam struct but this should be fine.
        let raw_post_field = post_field.map_or(ptr::null_mut(),|f| f
            .as_bytes()   // &str -> &[u8]
            .iter()       // iterate over the bytes
            .map(|&b| b as i8) // convert each byte to i8
            .collect::<Vec<i8>>()
            .as_mut_ptr() as *mut i8
        );
        // SAFETY: Because the struct in the C code is not defined as const char* we need to cast
        // the *const into *mut which should be safe because the function should not change it anyway
        let mut headers = self
            .headers
            .iter_mut()
            .map(|h| h.as_ptr() as *mut i8)
            .collect::<Vec<_>>();
        let data_len = self.data.as_ref().map_or(0, |d| d.len());
        let mut params = sys::FsNetworkHttpRequestParam {
            postField: raw_post_field,
            headerOptions: headers.as_mut_ptr(),
            headerOptionsSize: headers.len() as std::os::raw::c_uint,
            data: self
                .data
                .as_mut()
                .map_or(ptr::null_mut(), |d| d.as_mut_ptr()),
            dataSize: data_len as std::os::raw::c_uint,
        };
        let callback_data = self.callback.map_or(ptr::null_mut(), Box::into_raw) as *mut _;
        let request_id = unsafe {
            request(
                self.url.as_ptr(),
                &mut params as *mut sys::FsNetworkHttpRequestParam,
                Some(Self::c_wrapper),
                callback_data,
            )
        };
        if (request_id != 0) {
            if (id_exists(request_id as i32)) {
                println!("request_id: {}", request_id);
            }
        }
        if request_id == 0 {
            // Free the callback
            let _: Box<NetworkCallback> = unsafe { Box::from_raw(callback_data as *mut _) };
            None
        } else {
            if let Some(callback) = self.registered_callback.take() {
                register_callback(request_id as i32, callback);
            }
            Some(NetworkRequest(request_id))
        }
    }

    extern "C" fn c_wrapper(
        request_id: sys::FsNetworkRequestId,
        status_code: i32,
        user_data: *mut ffi::c_void,
    ) {
        if id_exists(request_id as i32) {
            let nr = NetworkRequest(request_id);
            let data_size: usize = nr.data_size();
            let code: i32 = nr.error_code();
            CALLBACK_REGISTRY.with(|registry| {
                let request_id32 = request_id as i32;
                if let Some(callback) = registry.borrow().get(&request_id32) {
                    let mut cb = callback.borrow_mut();
                    if (data_size <= 0) {
                        cb("".to_string(), code);
                    }
                    else{
                        let data = unsafe { sys::fsNetworkHttpRequestGetData(request_id) };
                        if !data.is_null() && nr.data_size() > 0 {
                            unsafe {
                                let bytes = slice::from_raw_parts(data, nr.data_size() as usize);
                                let mut bytesI8: Vec<u8> = bytes.into_iter().map(|b| *b as u8).collect();
                                if !bytesI8.ends_with(&[0]) {
                                    bytesI8.push(0);
                                }
                                let c_str = CString::from_vec_unchecked(bytesI8);
                                let response_string : String = c_str.into_string().unwrap();
                                let response_string :String = response_string.trim_end_matches(char::from(0)).to_string();
                                cb(response_string.clone(), code);
                            }
                        }else{
                            cb("".to_string(), code);
                        }
                    }
                } else {
                    println!("Callback not found for request_id: {}", request_id);
                }
            });

        } else if !user_data.is_null() {
            let callback: Box<NetworkCallback> = unsafe { Box::from_raw(user_data as *mut _) };
            callback(NetworkRequest(request_id), status_code);
        }
        else{
            eprintln!("callback not found for network request");
        }
    }
}

/// The states in which a network request can be in
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkRequestState {
    Invalid,
    New,
    WaitingForData,
    DataReady,
    Failed,
}
impl From<sys::FsNetworkHttpRequestState> for NetworkRequestState {
    fn from(value: sys::FsNetworkHttpRequestState) -> Self {
        match value {
            sys::FsNetworkHttpRequestState_FS_NETWORK_HTTP_REQUEST_STATE_INVALID => Self::Invalid,
            sys::FsNetworkHttpRequestState_FS_NETWORK_HTTP_REQUEST_STATE_NEW => Self::New,
            sys::FsNetworkHttpRequestState_FS_NETWORK_HTTP_REQUEST_STATE_WAITING_FOR_DATA => {
                Self::WaitingForData
            }
            sys::FsNetworkHttpRequestState_FS_NETWORK_HTTP_REQUEST_STATE_DATA_READY => {
                Self::DataReady
            }
            sys::FsNetworkHttpRequestState_FS_NETWORK_HTTP_REQUEST_STATE_FAILED => Self::Failed,
            _ => panic!("Unknown request state"),
        }
    }
}

/// Network request handle
#[derive(Clone, Copy)]
pub struct NetworkRequest(sys::FsNetworkRequestId);
impl NetworkRequest {
    /// Cancel a network request
    pub fn cancel(&self) -> bool {
        unsafe { sys::fsNetworkHttpCancelRequest(self.0) }
    }

    /// Get the size of the data
    pub fn data_size(&self) -> usize {
        unsafe { sys::fsNetworkHttpRequestGetDataSize(self.0) as usize }
    }

    /// Get the data
    pub fn data(&self) -> Option<Vec<u8>> {
        let data_size = self.data_size();
        if data_size == 0 {
            return None;
        }
        unsafe {
            let data = sys::fsNetworkHttpRequestGetData(self.0);
            if data.is_null() {
                None
            } else {
                let result = slice::from_raw_parts(data, data_size).to_owned();
                libc::free(data as *mut ffi::c_void);
                Some(result)
            }
        }
    }

    /// Get the HTTP status code or negative if the request failed
    pub fn error_code(&self) -> i32 {
        unsafe { sys::fsNetworkHttpRequestGetErrorCode(self.0) }
    }

    /// Get the current state of the request
    pub fn state(&self) -> NetworkRequestState {
        unsafe { sys::fsNetworkHttpRequestGetState(self.0).into() }
    }

    /// Get a specific header section
    pub fn header_section(&self, section: &str) -> Option<String> {
        let section = CString::new(section).ok()?;
        unsafe {
            let a = sys::fsNetworkHttpRequestGetHeaderSection(self.0, section.as_ptr());
            if a.is_null() {
                None
            } else {
                let result = CStr::from_ptr(a).to_str().ok().map(|s| s.to_owned());
                libc::free(a as *mut ffi::c_void);
                result
            }
        }
    }
}
