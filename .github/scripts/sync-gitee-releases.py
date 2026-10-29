#!/usr/bin/env python3
"""将单个 GitHub Release 的版本信息同步到 Gitee。

只同步标题、说明和预发布状态，不处理附件；Gitee 上手动上传的附件保持不变。
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


GITHUB_API = "https://api.github.com"
GITEE_API = "https://gitee.com/api/v5"
GITHUB_REPO = os.environ.get("GITHUB_REPOSITORY", "mosheng1/QcClip")
GITEE_REPO = os.environ["GITEE_REPO"]
GITHUB_TOKEN = os.environ["GH_TOKEN"]
GITEE_TOKEN = os.environ["GITEE_TOKEN"]
EVENT_NAME = os.environ.get("GITHUB_EVENT_NAME", "workflow_dispatch")
EVENT_PATH = Path(os.environ["GITHUB_EVENT_PATH"])
SYNC_TAG = os.environ.get("SYNC_TAG", "").strip()
API_RETRY_COUNT = 4
RETRYABLE_HTTP_STATUS = {408, 429, 500, 502, 503, 504}


def _retry_delay(attempt: int) -> int:
    """使用有限的指数退避，避免短时间内持续冲击 Gitee。"""
    return 2 ** attempt


def _is_retryable_error(error: Exception) -> bool:
    if isinstance(error, urllib.error.HTTPError):
        return error.code in RETRYABLE_HTTP_STATUS
    if isinstance(error, urllib.error.URLError):
        return True
    return isinstance(error, TimeoutError)


def request_json(
    base_url: str,
    path: str,
    token: str,
    method: str = "GET",
    payload: dict | None = None,
    gitee: bool = False,
):
    """调用 API，并在错误信息中隐藏令牌。"""
    url = f"{base_url}{path}"
    if gitee:
        separator = "&" if "?" in url else "?"
        url = f"{url}{separator}access_token={urllib.parse.quote(token)}"
    headers = {"Accept": "application/json", "User-Agent": "QcClip-release-sync"}
    if not gitee:
        headers["Authorization"] = f"Bearer {token}"
    data = None
    if payload is not None:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    retryable = method in {"GET", "HEAD", "PATCH", "DELETE"}
    for attempt in range(API_RETRY_COUNT):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                content = response.read()
                return json.loads(content) if content else None
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as error:
            if not retryable or not _is_retryable_error(error) or attempt == API_RETRY_COUNT - 1:
                if isinstance(error, urllib.error.HTTPError):
                    detail = error.read().decode("utf-8", errors="replace")[:500]
                    raise RuntimeError(
                        f"API 请求失败：{method} {path}，HTTP {error.code}，{detail}"
                    ) from error
                reason = error.reason if isinstance(error, urllib.error.URLError) else error
                raise RuntimeError(f"网络请求失败：{method} {path}，{reason}") from error
            delay = _retry_delay(attempt)
            print(
                f"API 请求暂时失败，将在 {delay} 秒后重试（{attempt + 1}/{API_RETRY_COUNT - 1}）："
                f"{method} {path}",
                flush=True,
            )
            time.sleep(delay)


def request_form(path: str, token: str, method: str, payload: dict) -> dict:
    """按 Gitee API 要求以 formData 提交 Release 参数。"""
    url = f"{GITEE_API}{path}?access_token={urllib.parse.quote(token)}"
    data = urllib.parse.urlencode(payload).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=data,
        headers={
            "Accept": "application/json",
            "Content-Type": "application/x-www-form-urlencoded",
            "User-Agent": "QcClip-release-sync",
        },
        method=method,
    )
    retryable = method in {"HEAD", "PATCH", "DELETE"}
    for attempt in range(API_RETRY_COUNT):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                content = response.read()
                return json.loads(content) if content else {}
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as error:
            if not retryable or not _is_retryable_error(error) or attempt == API_RETRY_COUNT - 1:
                if isinstance(error, urllib.error.HTTPError):
                    detail = error.read().decode("utf-8", errors="replace")[:500]
                    raise RuntimeError(
                        f"API 请求失败：{method} {path}，HTTP {error.code}，{detail}"
                    ) from error
                reason = error.reason if isinstance(error, urllib.error.URLError) else error
                raise RuntimeError(f"网络请求失败：{method} {path}，{reason}") from error
            delay = _retry_delay(attempt)
            print(
                f"API 请求暂时失败，将在 {delay} 秒后重试（{attempt + 1}/{API_RETRY_COUNT - 1}）："
                f"{method} {path}",
                flush=True,
            )
            time.sleep(delay)


def github_release(tag: str) -> dict:
    encoded_tag = urllib.parse.quote(tag, safe="")
    try:
        return request_json(
            GITHUB_API,
            f"/repos/{GITHUB_REPO}/releases/tags/{encoded_tag}",
            GITHUB_TOKEN,
        )
    except RuntimeError as error:
        if "HTTP 404" in str(error):
            raise RuntimeError(f"标签没有对应的已发布 GitHub Release：{tag}") from error
        raise


def gitee_release(tag: str) -> dict | None:
    page = 1
    while True:
        batch = request_json(
            GITEE_API,
            f"/repos/{GITEE_REPO}/releases?per_page=100&page={page}",
            GITEE_TOKEN,
            gitee=True,
        )
        if not batch:
            return None
        current = next((release for release in batch if release.get("tag_name") == tag), None)
        if current or len(batch) < 100:
            return current
        page += 1


def target_release() -> dict:
    """发布事件取事件中的 Release，手动运行取指定标签的 Release。"""
    if EVENT_NAME == "workflow_dispatch":
        if not SYNC_TAG:
            raise RuntimeError("手动同步需要指定版本标签")
        return github_release(SYNC_TAG)
    event = json.loads(EVENT_PATH.read_text(encoding="utf-8"))
    return request_json(
        GITHUB_API,
        f"/repos/{GITHUB_REPO}/releases/{event['release']['id']}",
        GITHUB_TOKEN,
    )


def sync_release(github_release: dict) -> None:
    tag = github_release["tag_name"]
    current = gitee_release(tag)
    payload = {
        "tag_name": tag,
        "name": github_release.get("name") or tag,
        "body": github_release.get("body") or "",
        "prerelease": bool(github_release.get("prerelease", False)),
    }
    if current:
        # 只修改版本信息，已存在的附件保留。
        request_form(
            f"/repos/{GITEE_REPO}/releases/{current['id']}",
            GITEE_TOKEN,
            method="PATCH",
            payload=payload,
        )
        print(f"已更新 Release：{tag}", flush=True)
    else:
        request_form(
            f"/repos/{GITEE_REPO}/releases",
            GITEE_TOKEN,
            method="POST",
            payload={
                **payload,
                "target_commitish": github_release.get("target_commitish") or tag,
            },
        )
        print(f"已创建 Release：{tag}，附件需在 Gitee 手动上传", flush=True)


def main() -> None:
    if not GITEE_TOKEN:
        raise RuntimeError("环境变量 GITEE_TOKEN 为空")
    release = target_release()
    if release.get("draft"):
        print(f"Release 仍是草稿，跳过同步：{release.get('tag_name', '未知版本')}", flush=True)
        return
    sync_release(release)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # noqa: BLE001 - 工作流需要输出可读的失败原因
        print(f"同步失败：{error}", file=sys.stderr)
        sys.exit(1)
