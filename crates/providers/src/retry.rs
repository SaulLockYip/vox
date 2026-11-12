//! 失败重试：**只对「重试可能有用」的错误重试**。
//!
//! 为什么需要：模型接口偶尔会 429（限流）或 5xx（服务端抖一下）。
//! 这类失败是**瞬时的**，重试一次就成了。以前是直接失败 → Vox 悄悄退回
//! 原始转写 → 用户看到「我问什么它就答什么」，完全不知道请求其实失败了。
//!
//! 反过来，401（没授权）、400（参数错）这类错误**重试多少次都没用**，
//! 所以只重试 [`vox_core::error::Error::is_transient`] 判定为瞬时的错误。

use std::future::Future;
use std::time::Duration;

use vox_core::error::Result;

/// 重试次数（含第一次）。
pub const DEFAULT_ATTEMPTS: usize = 3;

/// 指数退避的基准间隔。
pub const BASE_DELAY: Duration = Duration::from_millis(700);

/// 跑一个请求，失败且「瞬时失败」时按指数退避重试。
///
/// - `attempts`：总尝试次数（1 = 不重试）
/// - 每次重试前等待 `BASE_DELAY * 2^(已重试次数)`（0.7s → 1.4s → 2.8s）
pub async fn with_retry<F, Fut, T>(attempts: usize, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let attempts = attempts.max(1);

    for attempt in 0..attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let retryable = e.is_transient();
                if !retryable || attempt + 1 >= attempts {
                    return Err(e);
                }
                let wait = BASE_DELAY * 2u32.pow(attempt as u32);
                tracing::warn!(
                    error = %e,
                    attempt = attempt + 1,
                    "请求失败（可重试），等待 {:?} 后重试",
                    wait
                );
                tokio::time::sleep(wait).await;
            }
        }
    }

    // 理论上走不到这里（最后一次失败会 return），保底给个错误
    Err(vox_core::error::Error::Other("重试失败".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use vox_core::error::Error;

    #[tokio::test]
    async fn retries_transient_errors_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let out = with_retry(3, || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(Error::Api {
                        status: 429,
                        body: "限流".into(),
                    })
                } else {
                    Ok("成功")
                }
            }
        })
        .await;
        assert_eq!(out.unwrap(), "成功");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "应该试了 3 次");
    }

    #[tokio::test]
    async fn does_not_retry_permanent_errors() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let out: Result<()> = with_retry(3, || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(Error::Api {
                    status: 401,
                    body: "没授权".into(),
                })
            }
        })
        .await;
        assert!(out.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "401 不该重试");
    }
}
