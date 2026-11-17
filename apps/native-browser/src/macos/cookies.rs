fn native_chrome_cookie(
    cookie: &agent_protocol::browser_native::BrowserCookie,
) -> Result<objc2::rc::Retained<objc2_foundation::NSHTTPCookie>, String> {
    use objc2::runtime::AnyObject;
    use objc2_foundation::*;
    // Preserve Chrome's leading dot: cookie::Cookie::domain() strips it and changes host scope.
    // SAFETY: the framework property keys are immutable NSString constants;
    // each value has the NSString or NSDate type required by its cookie key.
    unsafe {
        let properties: objc2::rc::Retained<
            NSMutableDictionary<NSHTTPCookiePropertyKey, AnyObject>,
        > = NSMutableDictionary::from_slices(
            &[
                NSHTTPCookieName,
                NSHTTPCookieValue,
                NSHTTPCookieDomain,
                NSHTTPCookiePath,
            ],
            &[
                &*NSString::from_str(&cookie.name),
                &*NSString::from_str(&cookie.value),
                &*NSString::from_str(&cookie.domain),
                &*NSString::from_str(&cookie.path),
            ],
        );
        if cookie.secure {
            properties.insert(NSHTTPCookieSecure, ns_string!("TRUE"));
        }
        if cookie.http_only {
            properties.insert(ns_string!("HttpOnly"), ns_string!("TRUE"));
        }
        if let Some(expires) = cookie.expires {
            properties.insert(
                NSHTTPCookieExpires,
                &*NSDate::dateWithTimeIntervalSince1970(expires as f64),
            );
        } else {
            properties.insert(NSHTTPCookieDiscard, ns_string!("TRUE"));
        }
        match cookie.same_site {
            // Foundation represents unrestricted SameSite=None with nil.
            0 => {}
            // WebKit interprets nil as unrestricted; preserve Chrome's Lax-by-default policy.
            -1 | 1 => {
                properties.insert(NSHTTPCookieSameSitePolicy, NSHTTPCookieSameSiteLax);
            }
            2 => {
                properties.insert(NSHTTPCookieSameSitePolicy, NSHTTPCookieSameSiteStrict);
            }
            _ => return Err("CookieのSameSite属性が不正です。取り込みは行いませんでした。".into()),
        }
        NSHTTPCookie::cookieWithProperties(&properties)
            .ok_or_else(|| "WebViewに渡せないCookieがあります。取り込みは行いませんでした".into())
    }
}

pub(super) async fn install(
    store: &objc2_web_kit::WKHTTPCookieStore,
    cookies: Vec<agent_protocol::browser_native::BrowserCookie>,
) -> Result<(), String> {
    // Validate the whole batch before updating any existing browser cookie.
    let native = cookies
        .into_iter()
        .map(|cookie| native_chrome_cookie(&cookie))
        .collect::<Result<Vec<_>, _>>()?;
    let (tx, rx) = async_channel::unbounded();
    for cookie in &native {
        let tx = tx.clone();
        let completion = block2::RcBlock::new(move || {
            let _ = tx.try_send(());
        });
        // SAFETY: the cookie is a validated NSHTTPCookie. WebKit copies the
        // completion block, which owns its sender and can outlive this await.
        unsafe {
            store.setCookie_completionHandler(cookie, Some(&completion));
        }
        drop(completion);
        rx.recv()
            .await
            .map_err(|_| "Cookieの保存完了を確認できません")?;
    }
    let (tx, rx) = async_channel::bounded(1);
    let completion = block2::RcBlock::new(
        move |stored: std::ptr::NonNull<
            objc2_foundation::NSArray<objc2_foundation::NSHTTPCookie>,
        >| {
            let identity = |cookie: &objc2_foundation::NSHTTPCookie| {
                (
                    cookie.domain().to_string(),
                    cookie.path().to_string(),
                    cookie.name().to_string(),
                )
            };
            // SAFETY: WebKit keeps this non-null NSArray<NSHTTPCookie> alive
            // for the callback; the borrowed array is used only here.
            let stored = unsafe { stored.as_ref() };
            let stored: std::collections::HashMap<_, _> = stored
                .iter()
                .map(|cookie| (identity(&cookie), cookie))
                .collect();
            let matches = native.iter().all(|expected| {
                stored.get(&identity(expected)).is_some_and(|actual| {
                    actual.value() == expected.value()
                        && actual.isHTTPOnly() == expected.isHTTPOnly()
                        && actual.isSecure() == expected.isSecure()
                        && actual.isSessionOnly() == expected.isSessionOnly()
                        && actual.expiresDate() == expected.expiresDate()
                        && actual.sameSitePolicy() == expected.sameSitePolicy()
                })
            });
            let _ = tx.try_send(matches);
        },
    );
    // SAFETY: WebKit copies this completion block. It owns the expected cookies
    // and sender and reads the supplied array only during the callback.
    unsafe { store.getAllCookies(&completion) };
    drop(completion);
    if !rx
        .recv()
        .await
        .map_err(|_| "Cookieの保存結果を確認できません")?
    {
        return Err("一部のCookieを元の属性で保存できませんでした。保存済みのCookieもあります。再試行してください".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::browser_native::BrowserCookie;
    #[test]
    fn native_cookies_preserve_scope_and_authentication_protections() {
        let mut cookie = BrowserCookie {
            domain: ".example.test".into(),
            name: "login".into(),
            value: zeroize::Zeroizing::new("fixture".into()),
            path: "/account".into(),
            secure: true,
            http_only: true,
            same_site: 2,
            expires: Some(1_800_000_000),
        };
        for (site, expected) in [
            (-1, Some("lax")),
            (0, None),
            (1, Some("lax")),
            (2, Some("strict")),
        ] {
            cookie.same_site = site;
            let native = native_chrome_cookie(&cookie).unwrap();
            assert_eq!(native.domain().to_string(), ".example.test");
            assert_eq!(native.path().to_string(), "/account");
            assert!(native.isSecure());
            assert!(native.isHTTPOnly());
            assert!(!native.isSessionOnly());
            assert_eq!(
                native.expiresDate().unwrap().timeIntervalSince1970(),
                1_800_000_000.
            );
            assert_eq!(
                native
                    .sameSitePolicy()
                    .map(|s| s.to_string().to_lowercase()),
                expected.map(str::to_string)
            );
        }
        cookie.domain = "example.test".into();
        cookie.expires = None;
        cookie.same_site = 1;
        let native = native_chrome_cookie(&cookie).unwrap();
        assert_eq!(native.domain().to_string(), "example.test");
        assert!(native.isSessionOnly());
        cookie.same_site = 3;
        assert!(native_chrome_cookie(&cookie).is_err());
    }
}
