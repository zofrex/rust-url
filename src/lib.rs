// Copyright 2013-2015 Simon Sapin.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/*!

<a href="https://github.com/servo/rust-url"><img style="position: absolute; top: 0; left: 0; border: 0;" src="../github.png" alt="Fork me on GitHub"></a>
<style>.sidebar { margin-top: 53px }</style>

rust-url is an implementation of the [URL Standard](http://url.spec.whatwg.org/)
for the [Rust](http://rust-lang.org/) programming language.

It builds with [Cargo](http://crates.io/).
To use it in your project, add this to your `Cargo.toml` file:

```Cargo
[dependencies.url]
git = "https://github.com/servo/rust-url"
```

Supporting encodings other than UTF-8 in query strings is an optional feature
that requires [rust-encoding](https://github.com/lifthrasiir/rust-encoding)
and is off by default.
You can enable it with
[Cargo’s *features* mechanism](http://doc.crates.io/manifest.html#the-[features]-section):

```Cargo
[dependencies.url]
git = "https://github.com/servo/rust-url"
features = ["query_encoding"]
```

… or by passing `--cfg 'feature="query_encoding"'` to rustc.


# URL parsing and data structures

First, URL parsing may fail for various reasons and therefore returns a `Result`.

```
use url::{Url, ParseError};

assert!(Url::parse("http://[:::1]") == Err(ParseError::InvalidIpv6Address))
```

Let’s parse a valid URL and look at its components.

```
use url::{Url, Host};

let issue_list_url = Url::parse(
    "https://github.com/rust-lang/rust/issues?labels=E-easy&state=open"
).unwrap();


assert!(issue_list_url.scheme() == "https");
assert!(issue_list_url.username() == "");
assert!(issue_list_url.password() == None);
assert!(issue_list_url.host_str() == Some("github.com"));
assert!(issue_list_url.host() == Some(Host::Domain("github.com")));
assert!(issue_list_url.port() == None);
assert!(issue_list_url.path() == "/rust-lang/rust/issues");
assert!(issue_list_url.path_segments().map(|c| c.collect::<Vec<_>>()) ==
        Some(vec!["rust-lang", "rust", "issues"]));
assert!(issue_list_url.query() == Some("labels=E-easy&state=open"));
assert!(issue_list_url.fragment() == None);
assert!(!issue_list_url.non_relative());
```

Some URLs are said to be "non-relative":
they don’t have a username, password, host, or port,
and their "path" is an arbitrary string rather than slash-separated segments:

```
use url::Url;

let data_url = Url::parse("data:text/plain,Hello?World#").unwrap();

assert!(data_url.non_relative());
assert!(data_url.scheme() == "data");
assert!(data_url.path() == "text/plain,Hello");
assert!(data_url.path_segments().is_none());
assert!(data_url.query() == Some("World"));
assert!(data_url.fragment() == Some(""));
```


# Base URL

Many contexts allow URL *references* that can be relative to a *base URL*:

```html
<link rel="stylesheet" href="../main.css">
```

Since parsed URL are absolute, giving a base is required for parsing relative URLs:

```
use url::{Url, ParseError};

assert!(Url::parse("../main.css") == Err(ParseError::RelativeUrlWithoutBase))
```

Use the `join` method on an `Url` to use it as a base URL:

```
use url::Url;

let this_document = Url::parse("http://servo.github.io/rust-url/url/index.html").unwrap();
let css_url = this_document.join("../main.css").unwrap();
assert_eq!(css_url.as_str(), "http://servo.github.io/rust-url/main.css")
*/

#![cfg_attr(feature="heap_size", feature(plugin, custom_derive))]
#![cfg_attr(feature="heap_size", plugin(heapsize_plugin))]

#[cfg(feature="rustc-serialize")] extern crate rustc_serialize;
#[macro_use] extern crate matches;
#[cfg(feature="serde")] extern crate serde;
#[cfg(feature="heap_size")] #[macro_use] extern crate heapsize;

extern crate idna;

use host::HostInternal;
use parser::{Parser, Context};
use percent_encoding::{PATH_SEGMENT_ENCODE_SET, USERINFO_ENCODE_SET,
                       percent_encode, percent_decode, utf8_percent_encode};
use std::cmp;
use std::fmt::{self, Write};
use std::hash;
use std::io;
use std::mem;
use std::net::{ToSocketAddrs, Ipv4Addr, Ipv6Addr};
use std::ops::{Range, RangeFrom, RangeTo};
use std::path::{Path, PathBuf};
use std::str;

pub use encoding::EncodingOverride;
pub use origin::{Origin, OpaqueOrigin};
pub use host::{Host, HostAndPort, SocketAddrs};
pub use parser::{ParseError, to_u32};
pub use slicing::Position;
pub use webidl::WebIdl;

mod encoding;
mod host;
mod origin;
mod parser;
mod slicing;
mod webidl;

pub mod percent_encoding;
pub mod form_urlencoded;

/// A parsed URL record.
#[derive(Clone)]
#[cfg_attr(feature="heap_size", derive(HeapSizeOf))]
pub struct Url {
    /// Syntax in pseudo-BNF:
    ///
    ///   url = scheme ":" [ hierarchical | non-hierarchical ] [ "?" query ]? [ "#" fragment ]?
    ///   non-hierarchical = non-hierarchical-path
    ///   non-hierarchical-path = /* Does not start with "/" */
    ///   hierarchical = authority? hierarchical-path
    ///   authority = "//" userinfo? host [ ":" port ]?
    ///   userinfo = username [ ":" password ]? "@"
    ///   hierarchical-path = [ "/" path-segment ]+
    serialization: String,

    // Components
    scheme_end: u32,  // Before ':'
    username_end: u32,  // Before ':' (if a password is given) or '@' (if not)
    host_start: u32,
    host_end: u32,
    host: HostInternal,
    port: Option<u16>,
    path_start: u32,  // Before initial '/', if any
    query_start: Option<u32>,  // Before '?', unlike Position::QueryStart
    fragment_start: Option<u32>,  // Before '#', unlike Position::FragmentStart
}

#[derive(Default)]
pub struct ParseOptions<'a> {
    pub base_url: Option<&'a Url>,
    #[cfg(feature = "query_encoding")] pub encoding_override: Option<encoding::EncodingRef>,
    pub log_syntax_violation: Option<&'a Fn(&'static str)>,
}

impl Url {
    /// Parse an absolute URL from a string.
    #[inline]
    pub fn parse(input: &str) -> Result<Url, ::ParseError> {
        Url::parse_with(input, ParseOptions::default())
    }

    /// Parse a string as an URL, with this URL as the base URL.
    #[inline]
    pub fn join(&self, input: &str) -> Result<Url, ::ParseError> {
        Url::parse_with(input, ParseOptions { base_url: Some(self), ..Default::default() })
    }

    /// The URL parser with all of its parameters.
    ///
    /// `encoding_override` is a legacy concept only relevant for HTML.
    /// When it’s not needed,
    /// `s.parse::<Url>()`, `Url::from_str(s)` and `url.join(s)` can be used instead.
    pub fn parse_with(input: &str, options: ParseOptions) -> Result<Url, ::ParseError> {
        Parser {
            serialization: String::with_capacity(input.len()),
            base_url: options.base_url,
            query_encoding_override: EncodingOverride::from_parse_options(&options),
            log_syntax_violation: options.log_syntax_violation,
            context: Context::UrlParser,
        }.parse_url(input)
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.serialization
    }

    /// Return the scheme of this URL, lower-cased, as an ASCII string without the ':' delimiter.
    #[inline]
    pub fn scheme(&self) -> &str {
        self.slice(..self.scheme_end)
    }

    /// Return whether the URL has a host.
    #[inline]
    pub fn has_host(&self) -> bool {
        debug_assert!(self.byte_at(self.scheme_end) == b':');
        self.slice(self.scheme_end + 1 ..).starts_with("//")
    }

    /// Return whether this URL is non-relative (typical of e.g. `data:` and `mailto:` URLs.)
    #[inline]
    pub fn non_relative(&self) -> bool {
        self.byte_at(self.path_start) != b'/'
    }

    /// Return the username for this URL (typically the empty string)
    /// as a percent-encoded ASCII string.
    pub fn username(&self) -> &str {
        if self.has_host() {
            self.slice(self.scheme_end + 3..self.username_end)
        } else {
            ""
        }
    }

    /// Return the password for this URL, if any, as a percent-encoded ASCII string.
    pub fn password(&self) -> Option<&str> {
        // This ':' is not the one marking a port number since a host can not be empty.
        // (Except for file: URLs, which do not have port numbers.)
        if self.byte_at(self.username_end) == b':' {
            debug_assert!(self.has_host());
            debug_assert!(self.host_start < self.host_end);
            debug_assert!(self.byte_at(self.host_start - 1) == b'@');
            Some(self.slice(self.username_end + 1..self.host_start - 1))
        } else {
            None
        }
    }

    /// Return the string representation of the host (domain or IP address) for this URL, if any.
    ///
    /// Non-ASCII domains are punycode-encoded per IDNA.
    /// IPv6 addresses are given between `[` and `]` brackets.
    ///
    /// Non-relative URLs (typical of `data:` and `mailto:`) and some `file:` URLs
    /// don’t have a host.
    ///
    /// See also the `host` method.
    pub fn host_str(&self) -> Option<&str> {
        if self.has_host() {
            Some(self.slice(self.host_start..self.host_end))
        } else {
            None
        }
    }

    /// Return the parsed representation of the host for this URL.
    /// Non-ASCII domain labels are punycode-encoded per IDNA.
    ///
    /// Non-relative URLs (typical of `data:` and `mailto:`) and some `file:` URLs
    /// don’t have a host.
    ///
    /// See also the `host_str` method.
    pub fn host(&self) -> Option<Host<&str>> {
        match self.host {
            HostInternal::None => None,
            HostInternal::Domain => Some(Host::Domain(self.slice(self.host_start..self.host_end))),
            HostInternal::Ipv4(address) => Some(Host::Ipv4(address)),
            HostInternal::Ipv6(address) => Some(Host::Ipv6(address)),
        }
    }

    /// If this URL has a host and it is a domain name (not an IP address), return it.
    pub fn domain(&self) -> Option<&str> {
        match self.host {
            HostInternal::Domain => Some(self.slice(self.host_start..self.host_end)),
            _ => None,
        }
    }

    /// Return the port number for this URL, if any.
    #[inline]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Return the port number for this URL, or the default port number if it is known.
    ///
    /// This method only knows the default port number
    /// of the `http`, `https`, `ws`, `wss`, `ftp`, and `gopher` schemes.
    ///
    /// For URLs in these schemes, this method always returns `Some(_)`.
    /// For other schemes, it is the same as `Url::port()`.
    #[inline]
    pub fn port_or_known_default(&self) -> Option<u16> {
        self.port.or_else(|| parser::default_port(self.scheme()))
    }

    /// If the URL has a host, return something that implements `ToSocketAddrs`.
    ///
    /// If the URL has no port number and the scheme’s default port number is not known
    /// (see `Url::port_or_known_default`),
    /// the closure is called to obtain a port number.
    /// Typically, this closure can match on the result `Url::scheme`
    /// to have per-scheme default port numbers,
    /// and panic for schemes it’s not prepared to handle.
    /// For example:
    ///
    /// ```rust
    /// # use url::Url;
    /// # use std::net::TcpStream;
    /// # use std::io;
    ///
    /// fn connect(url: &Url) -> io::Result<TcpStream> {
    ///     TcpStream::connect(try!(url.with_default_port(default_port)))
    /// }
    ///
    /// fn default_port(url: &Url) -> Result<u16, ()> {
    ///     match url.scheme() {
    ///         "git" => Ok(9418),
    ///         "git+ssh" => Ok(22),
    ///         "git+https" => Ok(443),
    ///         "git+http" => Ok(80),
    ///         _ => Err(()),
    ///     }
    /// }
    /// ```
    pub fn with_default_port<F>(&self, f: F) -> io::Result<HostAndPort<&str>>
    where F: FnOnce(&Url) -> Result<u16, ()> {
        Ok(HostAndPort {
            host: try!(self.host()
                           .ok_or(())
                           .or_else(|()| io_error("URL has no host"))),
            port: try!(self.port_or_known_default()
                           .ok_or(())
                           .or_else(|()| f(self))
                           .or_else(|()| io_error("URL has no port number")))
        })
    }

    /// Return the path for this URL, as a percent-encoded ASCII string.
    /// For relative URLs, this starts with a '/' slash
    /// and continues with slash-separated path segments.
    /// For non-relative URLs, this is an arbitrary string that doesn’t start with '/'.
    pub fn path(&self) -> &str {
        match (self.query_start, self.fragment_start) {
            (None, None) => self.slice(self.path_start..),
            (Some(next_component_start), _) |
            (None, Some(next_component_start)) => {
                self.slice(self.path_start..next_component_start)
            }
        }
    }

    /// If this URL is relative, return an iterator of '/' slash-separated path segments,
    /// each as a percent-encoded ASCII string.
    ///
    /// Return `None` for non-relative URLs, or an iterator of at least one string.
    pub fn path_segments(&self) -> Option<str::Split<char>> {
        let path = self.path();
        if path.starts_with('/') {
            Some(path[1..].split('/'))
        } else {
            None
        }
    }

    /// Return this URL’s query string, if any, as a percent-encoded ASCII string.
    pub fn query(&self) -> Option<&str> {
        match (self.query_start, self.fragment_start) {
            (None, _) => None,
            (Some(query_start), None) => {
                debug_assert!(self.byte_at(query_start) == b'?');
                Some(self.slice(query_start + 1..))
            }
            (Some(query_start), Some(fragment_start)) => {
                debug_assert!(self.byte_at(query_start) == b'?');
                Some(self.slice(query_start + 1..fragment_start))
            }
        }
    }

    /// Return this URL’s fragment identifier, if any.
    ///
    /// **Note:** the parser did *not* percent-encode this component,
    /// but the input may have been percent-encoded already.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment_start.map(|start| {
            debug_assert!(self.byte_at(start) == b'#');
            self.slice(start + 1..)
        })
    }

    fn mutate<F: FnOnce(&mut Parser) -> R, R>(&mut self, f: F) -> R {
        let mut parser = Parser::for_setter(mem::replace(&mut self.serialization, String::new()));
        let result = f(&mut parser);
        self.serialization = parser.serialization;
        result
    }

    /// Change this URL’s fragment identifier.
    pub fn set_fragment(&mut self, fragment: Option<&str>) {
        // Remove any previous fragment
        if let Some(start) = self.fragment_start {
            debug_assert!(self.byte_at(start) == b'#');
            self.serialization.truncate(start as usize);
        }
        // Write the new one
        if let Some(input) = fragment {
            self.fragment_start = Some(to_u32(self.serialization.len()).unwrap());
            self.serialization.push('#');
            self.mutate(|parser| parser.parse_fragment(input))
        } else {
            self.fragment_start = None
        }
    }

    /// Change this URL’s query string.
    pub fn set_query(&mut self, query: Option<&str>) {
        // Stash any fragment
        let fragment = self.fragment_start.map(|start| {
            let f = self.slice(start..).to_owned();
            self.serialization.truncate(start as usize);
            f
        });
        // Remove any previous query
        if let Some(start) = self.query_start {
            debug_assert!(self.byte_at(start) == b'?');
            self.serialization.truncate(start as usize);
        }
        // Write the new one
        if let Some(input) = query {
            self.query_start = Some(to_u32(self.serialization.len()).unwrap());
            self.serialization.push('?');
            let scheme_end = self.scheme_end;
            self.mutate(|parser| parser.parse_query(scheme_end, input));
        }
        // Restore the fragment, if any
        if let Some(ref fragment) = fragment {
            self.fragment_start = Some(to_u32(self.serialization.len()).unwrap());
            debug_assert!(fragment.starts_with('#'));
            self.serialization.push_str(fragment)  // It’s already been through the parser
        }
    }

    /// Change this URL’s path.
    pub fn set_path(&mut self, path: &str) {
        let (old_after_path_pos, after_path) = match (self.query_start, self.fragment_start) {
            (Some(i), _) | (None, Some(i)) => (i, self.slice(i..).to_owned()),
            (None, None) => (to_u32(self.serialization.len()).unwrap(), String::new())
        };
        let non_relative = self.non_relative();
        let scheme_type = parser::SchemeType::from(self.scheme());
        self.serialization.truncate(self.path_start as usize);
        self.mutate(|parser| {
            if non_relative {
                if path.starts_with('/') {
                    parser.serialization.push_str("%2F");
                    parser.parse_non_relative_path(&path[1..]);
                } else {
                    parser.parse_non_relative_path(path);
                }
            } else {
                let mut has_host = true;  // FIXME
                parser.parse_path_start(scheme_type, &mut has_host, path);
            }
        });
        let new_after_path_pos = to_u32(self.serialization.len()).unwrap();
        let adjust = |index: &mut u32| {
            *index -= old_after_path_pos;
            *index += new_after_path_pos;
        };
        if let Some(ref mut index) = self.query_start { adjust(index) }
        if let Some(ref mut index) = self.fragment_start { adjust(index) }
        self.serialization.push_str(&after_path)
    }

    /// Remove the last segment of this URL’s path.
    ///
    /// If this URL is non-relative, do nothing and return `Err`.
    pub fn pop_path_segment(&mut self) -> Result<(), ()> {
        if self.non_relative() {
            return Err(())
        }
        let last_slash;
        let path_len;
        {
            let path = self.path();
            last_slash = path.rfind('/').unwrap();
            path_len = path.len();
        };
        if last_slash > 0 {
            // Found a slash other than the initial one
            let last_slash = last_slash + self.path_start as usize;
            let path_end = path_len + self.path_start as usize;
            unsafe {
                self.serialization.as_mut_vec().drain(last_slash..path_end);
            }
            let offset = (path_end - last_slash) as u32;
            if let Some(ref mut index) = self.query_start { *index -= offset }
            if let Some(ref mut index) = self.fragment_start { *index -= offset }
        }
        Ok(())
    }

    /// Add a segment at the end of this URL’s path.
    ///
    /// If this URL is non-relative, do nothing and return `Err`.
    pub fn push_path_segment(&mut self, segment: &str) -> Result<(), ()> {
        if self.non_relative() {
            return Err(())
        }
        let after_path = match (self.query_start, self.fragment_start) {
            (Some(i), _) | (None, Some(i)) => {
                let s = self.slice(i..).to_owned();
                self.serialization.truncate(i as usize);
                s
            },
            (None, None) => String::new()
        };
        let scheme_type = parser::SchemeType::from(self.scheme());
        let path_start = self.path_start as usize;
        self.serialization.push('/');
        self.mutate(|parser| {
            parser.context = parser::Context::PathSegmentSetter;
            let mut has_host = true;  // FIXME account for this?
            parser.parse_path(scheme_type, &mut has_host, path_start, segment)
        });
        let offset = to_u32(self.serialization.len()).unwrap() - self.path_start;
        if let Some(ref mut index) = self.query_start { *index += offset }
        if let Some(ref mut index) = self.fragment_start { *index += offset }
        self.serialization.push_str(&after_path);
        Ok(())
    }

    /// Change this URL’s port number.
    ///
    /// If this URL is non-relative, does not have a host, or has the `file` scheme;
    /// do nothing and return `Err`.
    pub fn set_port(&mut self, mut port: Option<u16>) -> Result<(), ()> {
        if !self.has_host() || self.scheme() == "file" {
            return Err(())
        }
        if port.is_some() && port == parser::default_port(self.scheme()) {
            port = None
        }
        self.set_port_internal(port);
        Ok(())
    }

    fn set_port_internal(&mut self, port: Option<u16>) {
        match (self.port, port) {
            (None, None) => {}
            (Some(_), None) => {
                unsafe {
                    self.serialization.as_mut_vec().drain(
                        self.host_end as usize .. self.path_start as usize);
                }
                let offset = self.path_start - self.host_end;
                self.path_start = self.host_end;
                if let Some(ref mut index) = self.query_start { *index -= offset }
                if let Some(ref mut index) = self.fragment_start { *index -= offset }
            }
            (Some(old), Some(new)) if old == new => {}
            (_, Some(new)) => {
                let path_and_after = self.slice(self.path_start..).to_owned();
                self.serialization.truncate(self.host_end as usize);
                write!(&mut self.serialization, ":{}", new).unwrap();
                let old_path_start = self.path_start;
                let new_path_start = to_u32(self.serialization.len()).unwrap();
                self.path_start = new_path_start;
                let adjust = |index: &mut u32| {
                    *index -= old_path_start;
                    *index += new_path_start;
                };
                if let Some(ref mut index) = self.query_start { adjust(index) }
                if let Some(ref mut index) = self.fragment_start { adjust(index) }
                self.serialization.push_str(&path_and_after);
            }
        }
    }

    /// Change this URL’s host.
    ///
    /// If this URL is non-relative or there is an error parsing the given `host`,
    /// do nothing and return `Err`.
    ///
    /// Removing the host (calling this with `None`)
    /// will also remove any username, password, and port number.
    pub fn set_host(&mut self, host: Option<&str>) -> Result<(), ()> {
        if self.non_relative() {
            return Err(())
        }

        if let Some(host) = host {
            self.set_host_internal(try!(Host::parse(host).map_err(|_| ())), None)
        } else if self.has_host() {
            // Not debug_assert! since this proves that `unsafe` below is OK:
            assert!(self.byte_at(self.scheme_end) == b':');
            assert!(self.byte_at(self.path_start) == b'/');
            let new_path_start = self.scheme_end + 1;
            unsafe {
                self.serialization.as_mut_vec()
                    .drain(self.path_start as usize..new_path_start as usize);
            }
            let offset = self.path_start - new_path_start;
            self.path_start = new_path_start;
            self.username_end = new_path_start;
            self.host_start = new_path_start;
            self.host_end = new_path_start;
            self.port = None;
            if let Some(ref mut index) = self.query_start { *index -= offset }
            if let Some(ref mut index) = self.fragment_start { *index -= offset }
        }
        Ok(())
    }

    /// opt_new_port: None means leave unchanged, Some(None) means remove any port number.
    fn set_host_internal(&mut self, host: Host<String>, opt_new_port: Option<Option<u16>>) {
        let old_suffix_pos = if opt_new_port.is_some() { self.path_start } else { self.host_end };
        let suffix = self.slice(old_suffix_pos..).to_owned();
        self.serialization.truncate(self.host_start as usize);
        if !self.has_host() {
            debug_assert!(self.slice(self.scheme_end..self.host_start) == ":");
            debug_assert!(self.username_end == self.host_start);
            self.serialization.push('/');
            self.serialization.push('/');
            self.username_end += 2;
            self.host_start += 2;
        }
        write!(&mut self.serialization, "{}", host).unwrap();
        self.host_end = to_u32(self.serialization.len()).unwrap();
        self.host = host.into();

        if let Some(new_port) = opt_new_port {
            self.port = new_port;
            if let Some(port) = new_port {
                write!(&mut self.serialization, ":{}", port).unwrap();
            }
        }
        let new_suffix_pos = to_u32(self.serialization.len()).unwrap();
        self.serialization.push_str(&suffix);

        let adjust = |index: &mut u32| {
            *index -= old_suffix_pos;
            *index += new_suffix_pos;
        };
        adjust(&mut self.path_start);
        if let Some(ref mut index) = self.query_start { adjust(index) }
        if let Some(ref mut index) = self.fragment_start { adjust(index) }
    }

    /// Change this URL’s host to the given IPv4 address.
    ///
    /// If this URL is non-relative, do nothing and return `Err`.
    ///
    /// Compared to `Url::set_host`, this skips the host parser.
    pub fn set_ipv4_host(&mut self, address: Ipv4Addr) -> Result<(), ()> {
        if self.non_relative() {
            return Err(())
        }

        self.set_host_internal(Host::Ipv4(address), None);
        Ok(())
    }

    /// Change this URL’s host to the given IPv6 address.
    ///
    /// If this URL is non-relative, do nothing and return `Err`.
    ///
    /// Compared to `Url::set_host`, this skips the host parser.
    pub fn set_ipv6_host(&mut self, address: Ipv6Addr) -> Result<(), ()> {
        if self.non_relative() {
            return Err(())
        }

        self.set_host_internal(Host::Ipv6(address), None);
        Ok(())
    }

    /// Change this URL’s password.
    ///
    /// If this URL is non-relative or does not have a host, do nothing and return `Err`.
    pub fn set_password(&mut self, password: Option<&str>) -> Result<(), ()> {
        if !self.has_host() {
            return Err(())
        }
        if let Some(password) = password {
            let host_and_after = self.slice(self.host_start..).to_owned();
            self.serialization.truncate(self.username_end as usize);
            self.serialization.push(':');
            self.serialization.extend(utf8_percent_encode(password, USERINFO_ENCODE_SET));
            self.serialization.push('@');

            let old_host_start = self.host_start;
            let new_host_start = to_u32(self.serialization.len()).unwrap();
            let adjust = |index: &mut u32| {
                *index -= old_host_start;
                *index += new_host_start;
            };
            self.host_start = new_host_start;
            adjust(&mut self.host_end);
            adjust(&mut self.path_start);
            if let Some(ref mut index) = self.query_start { adjust(index) }
            if let Some(ref mut index) = self.fragment_start { adjust(index) }

            self.serialization.push_str(&host_and_after);
        } else if self.byte_at(self.username_end) == b':' {  // If there is a password to remove
            let has_username_or_password = self.byte_at(self.host_start - 1) == b'@';
            debug_assert!(has_username_or_password);
            let username_start = self.scheme_end + 3;
            let empty_username = username_start == self.username_end;
            let start = self.username_end;  // Remove the ':'
            let end = if empty_username {
                self.host_start // Remove the '@' as well
            } else {
                self.host_start - 1  // Keep the '@' to separate the username from the host
            };
            unsafe {
                self.serialization.as_mut_vec().drain(start as usize .. end as usize);
            }
            let offset = end - start;
            self.host_start -= offset;
            self.host_end -= offset;
            if let Some(ref mut index) = self.query_start { *index -= offset }
            if let Some(ref mut index) = self.fragment_start { *index -= offset }
        }
        Ok(())
    }

    /// Change this URL’s username.
    ///
    /// If this URL is non-relative or does not have a host, do nothing and return `Err`.
    pub fn set_username(&mut self, username: &str) -> Result<(), ()> {
        if !self.has_host() {
            return Err(())
        }
        let username_start = self.scheme_end + 3;
        if self.slice(username_start..self.username_end) == username {
            return Ok(())
        }
        let after_username = self.slice(self.username_end..).to_owned();
        self.serialization.truncate(username_start as usize);
        self.serialization.extend(utf8_percent_encode(username, USERINFO_ENCODE_SET));

        let old_username_end = self.username_end;
        let new_username_end = to_u32(self.serialization.len()).unwrap();
        let adjust = |index: &mut u32| {
            *index -= old_username_end;
            *index += new_username_end;
        };

        self.username_end = new_username_end;
        adjust(&mut self.host_start);
        adjust(&mut self.host_end);
        adjust(&mut self.path_start);
        if let Some(ref mut index) = self.query_start { adjust(index) }
        if let Some(ref mut index) = self.fragment_start { adjust(index) }

        if !after_username.starts_with(|c| matches!(c, '@' | ':')) {
            self.serialization.push('@');
        }
        self.serialization.push_str(&after_username);
        Ok(())
    }

    /// Change this URL’s scheme.
    ///
    /// Do nothing and return `Err` if:
    /// * The new scheme is not in `[a-zA-Z][a-zA-Z0-9+.-]+`
    /// * This URL is non-relative and the new scheme is one of
    ///   `http`, `https`, `ws`, `wss`, `ftp`, or `gopher`
    pub fn set_scheme(&mut self, scheme: &str) -> Result<(), ()> {
        self.set_scheme_internal(scheme, false)
    }

    fn set_scheme_internal(&mut self, scheme: &str, allow_extra_input_after_colon: bool)
                          -> Result<(), ()> {
        let mut parser = Parser::for_setter(String::new());
        let remaining = try!(parser.parse_scheme(scheme));
        if !(remaining.is_empty() || allow_extra_input_after_colon) {
            return Err(())
        }
        let old_scheme_end = self.scheme_end;
        let new_scheme_end = to_u32(parser.serialization.len()).unwrap();
        let adjust = |index: &mut u32| {
            *index -= old_scheme_end;
            *index += new_scheme_end;
        };

        self.scheme_end = new_scheme_end;
        adjust(&mut self.username_end);
        adjust(&mut self.host_start);
        adjust(&mut self.host_end);
        adjust(&mut self.path_start);
        if let Some(ref mut index) = self.query_start { adjust(index) }
        if let Some(ref mut index) = self.fragment_start { adjust(index) }

        parser.serialization.push_str(self.slice(old_scheme_end..));
        self.serialization = parser.serialization;
        Ok(())
    }

    /// Convert a file name as `std::path::Path` into an URL in the `file` scheme.
    ///
    /// This returns `Err` if the given path is not absolute or,
    /// on Windows, if the prefix is not a disk prefix (e.g. `C:`).
    pub fn from_file_path<P: AsRef<Path>>(path: P) -> Result<Url, ()> {
        let mut serialization = "file://".to_owned();
        let path_start = serialization.len() as u32;
        try!(path_to_file_url_segments(path.as_ref(), &mut serialization));
        Ok(Url {
            serialization: serialization,
            scheme_end: "file".len() as u32,
            username_end: path_start,
            host_start: path_start,
            host_end: path_start,
            host: HostInternal::None,
            port: None,
            path_start: path_start,
            query_start: None,
            fragment_start: None,
        })
    }

    /// Convert a directory name as `std::path::Path` into an URL in the `file` scheme.
    ///
    /// This returns `Err` if the given path is not absolute or,
    /// on Windows, if the prefix is not a disk prefix (e.g. `C:`).
    ///
    /// Compared to `from_file_path`, this ensure that URL’s the path has a trailing slash
    /// so that the entire path is considered when using this URL as a base URL.
    ///
    /// For example:
    ///
    /// * `"index.html"` parsed with `Url::from_directory_path(Path::new("/var/www"))`
    ///   as the base URL is `file:///var/www/index.html`
    /// * `"index.html"` parsed with `Url::from_file_path(Path::new("/var/www"))`
    ///   as the base URL is `file:///var/index.html`, which might not be what was intended.
    ///
    /// Note that `std::path` does not consider trailing slashes significant
    /// and usually does not include them (e.g. in `Path::parent()`).
    pub fn from_directory_path<P: AsRef<Path>>(path: P) -> Result<Url, ()> {
        let mut url = try!(Url::from_file_path(path));
        if !url.serialization.ends_with('/') {
            url.serialization.push('/')
        }
        Ok(url)
    }

    /// Assuming the URL is in the `file` scheme or similar,
    /// convert its path to an absolute `std::path::Path`.
    ///
    /// **Note:** This does not actually check the URL’s `scheme`,
    /// and may give nonsensical results for other schemes.
    /// It is the user’s responsibility to check the URL’s scheme before calling this.
    ///
    /// ```
    /// # use url::Url;
    /// # let url = Url::parse("file:///etc/passwd").unwrap();
    /// let path = url.to_file_path();
    /// ```
    ///
    /// Returns `Err` if the host is neither empty nor `"localhost"`,
    /// or if `Path::new_opt()` returns `None`.
    /// (That is, if the percent-decoded path contains a NUL byte or,
    /// for a Windows path, is not UTF-8.)
    #[inline]
    pub fn to_file_path(&self) -> Result<PathBuf, ()> {
        // FIXME: Figure out what to do w.r.t host.
        if matches!(self.host(), None | Some(Host::Domain("localhost"))) {
            if let Some(segments) = self.path_segments() {
                return file_url_segments_to_pathbuf(segments)
            }
        }
        Err(())
    }

    /// Parse the URL’s query string, if any, as `application/x-www-form-urlencoded`
    /// and return a vector of (key, value) pairs.
    #[inline]
    pub fn query_pairs(&self) -> Option<Vec<(String, String)>> {
        self.query().map(|query| form_urlencoded::parse(query.as_bytes()))
    }

    // Private helper methods:

    #[inline]
    fn slice<R>(&self, range: R) -> &str where R: RangeArg {
        range.slice_of(&self.serialization)
    }

    #[inline]
    fn byte_at(&self, i: u32) -> u8 {
        self.serialization.as_bytes()[i as usize]
    }
}

/// Return an error if `Url::host` or `Url::port_or_known_default` return `None`.
impl ToSocketAddrs for Url {
    type Iter = SocketAddrs;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter> {
        try!(self.with_default_port(|_| Err(()))).to_socket_addrs()
    }
}

/// Parse a string as an URL, without a base URL or encoding override.
impl str::FromStr for Url {
    type Err = ParseError;

    #[inline]
    fn from_str(input: &str) -> Result<Url, ::ParseError> {
        Url::parse(input)
    }
}

/// Display the serialization of this URL.
impl fmt::Display for Url {
    #[inline]
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.serialization, formatter)
    }
}

/// Debug the serialization of this URL.
impl fmt::Debug for Url {
    #[inline]
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.serialization, formatter)
    }
}

/// URLs compare like their serialization.
impl Eq for Url {}

/// URLs compare like their serialization.
impl PartialEq for Url {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.serialization == other.serialization
    }
}

/// URLs compare like their serialization.
impl Ord for Url {
    #[inline]
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.serialization.cmp(&other.serialization)
    }
}

/// URLs compare like their serialization.
impl PartialOrd for Url {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        self.serialization.partial_cmp(&other.serialization)
    }
}

/// URLs hash like their serialization.
impl hash::Hash for Url {
    #[inline]
    fn hash<H>(&self, state: &mut H) where H: hash::Hasher {
        hash::Hash::hash(&self.serialization, state)
    }
}

/// Return the serialization of this URL.
impl AsRef<str> for Url {
    #[inline]
    fn as_ref(&self) -> &str {
        &self.serialization
    }
}

trait RangeArg {
    fn slice_of<'a>(&self, s: &'a str) -> &'a str;
}

impl RangeArg for Range<u32> {
    #[inline]
    fn slice_of<'a>(&self, s: &'a str) -> &'a str {
        &s[self.start as usize .. self.end as usize]
    }
}

impl RangeArg for RangeFrom<u32> {
    #[inline]
    fn slice_of<'a>(&self, s: &'a str) -> &'a str {
        &s[self.start as usize ..]
    }
}

impl RangeArg for RangeTo<u32> {
    #[inline]
    fn slice_of<'a>(&self, s: &'a str) -> &'a str {
        &s[.. self.end as usize]
    }
}

#[cfg(feature="rustc-serialize")]
impl rustc_serialize::Encodable for Url {
    fn encode<S: rustc_serialize::Encoder>(&self, encoder: &mut S) -> Result<(), S::Error> {
        encoder.emit_str(self.as_str())
    }
}


#[cfg(feature="rustc-serialize")]
impl rustc_serialize::Decodable for Url {
    fn decode<D: rustc_serialize::Decoder>(decoder: &mut D) -> Result<Url, D::Error> {
        Url::parse(&*try!(decoder.read_str())).map_err(|error| {
            decoder.error(&format!("URL parsing error: {}", error))
        })
    }
}

/// Serializes this URL into a `serde` stream.
///
/// This implementation is only available if the `serde` Cargo feature is enabled.
#[cfg(feature="serde")]
impl serde::Serialize for Url {
    fn serialize<S>(&self, serializer: &mut S) -> Result<(), S::Error> where S: serde::Serializer {
        format!("{}", self).serialize(serializer)
    }
}

/// Deserializes this URL from a `serde` stream.
///
/// This implementation is only available if the `serde` Cargo feature is enabled.
#[cfg(feature="serde")]
impl serde::Deserialize for Url {
    fn deserialize<D>(deserializer: &mut D) -> Result<Url, D::Error> where D: serde::Deserializer {
        let string_representation: String = try!(serde::Deserialize::deserialize(deserializer));
        Ok(Url::parse(&string_representation).unwrap())
    }
}

#[cfg(unix)]
fn path_to_file_url_segments(path: &Path, serialization: &mut String) -> Result<(), ()> {
    use std::os::unix::prelude::OsStrExt;
    if !path.is_absolute() {
        return Err(())
    }
    // skip the root component
    for component in path.components().skip(1) {
        serialization.push('/');
        serialization.extend(percent_encode(
            component.as_os_str().as_bytes(), PATH_SEGMENT_ENCODE_SET))
    }
    Ok(())
}

#[cfg(windows)]
fn path_to_file_url_segments(path: &Path, serialization: &mut String) -> Result<(), ()> {
    path_to_file_url_segments_windows(path, serialization)
}

// Build this unconditionally to alleviate https://github.com/servo/rust-url/issues/102
#[cfg_attr(not(windows), allow(dead_code))]
fn path_to_file_url_segments_windows(path: &Path, serialization: &mut String) -> Result<(), ()> {
    use std::path::{Prefix, Component};
    if !path.is_absolute() {
        return Err(())
    }
    let mut components = path.components();
    let disk = match components.next() {
        Some(Component::Prefix(ref p)) => match p.kind() {
            Prefix::Disk(byte) => byte,
            Prefix::VerbatimDisk(byte) => byte,
            _ => return Err(()),
        },

        // FIXME: do something with UNC and other prefixes?
        _ => return Err(())
    };

    // Start with the prefix, e.g. "C:"
    serialization.push('/');
    serialization.push(disk as char);
    serialization.push(':');

    for component in components {
        if component == Component::RootDir { continue }
        // FIXME: somehow work with non-unicode?
        let component = try!(component.as_os_str().to_str().ok_or(()));
        serialization.push('/');
        serialization.extend(percent_encode(component.as_bytes(), PATH_SEGMENT_ENCODE_SET));
    }
    Ok(())
}

#[cfg(unix)]
fn file_url_segments_to_pathbuf(segments: str::Split<char>) -> Result<PathBuf, ()> {
    use std::ffi::OsStr;
    use std::os::unix::prelude::OsStrExt;
    use std::path::PathBuf;

    let mut bytes = Vec::new();
    for segment in segments {
        bytes.push(b'/');
        bytes.extend(percent_decode(segment.as_bytes()));
    }
    let os_str = OsStr::from_bytes(&bytes);
    let path = PathBuf::from(os_str);
    debug_assert!(path.is_absolute(),
                  "to_file_path() failed to produce an absolute Path");
    Ok(path)
}

#[cfg(windows)]
fn file_url_segments_to_pathbuf(segments: str::Split<char>) -> Result<PathBuf, ()> {
    file_url_segments_to_pathbuf_windows(segments)
}

// Build this unconditionally to alleviate https://github.com/servo/rust-url/issues/102
#[cfg_attr(not(windows), allow(dead_code))]
fn file_url_segments_to_pathbuf_windows(mut segments: str::Split<char>) -> Result<PathBuf, ()> {
    let first = try!(segments.next().ok_or(()));
    if first.len() != 2 || !first.starts_with(parser::ascii_alpha)
            || first.as_bytes()[1] != b':' {
        return Err(())
    }
    let mut string = first.to_owned();
    for segment in segments {
        string.push('\\');

        // Currently non-unicode windows paths cannot be represented
        match String::from_utf8(percent_decode(segment.as_bytes()).collect()) {
            Ok(s) => string.push_str(&s),
            Err(..) => return Err(()),
        }
    }
    let path = PathBuf::from(string);
    debug_assert!(path.is_absolute(),
                  "to_file_path() failed to produce an absolute Path");
    Ok(path)
}

fn io_error<T>(reason: &str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidData, reason))
}
