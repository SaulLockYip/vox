#!/usr/bin/env python3
"""本地假 ASR / LLM 服务器 —— 让 Vox 的全链路可以**确定性复现**。

用途：
- 自动化测试时接管 /v1/speech_to_text 与 /v1/messages，
  真实录音**不会离开这台机器**（也不需要真的调 MiniMax）。
- 把每次收到的 prompt 落到 /tmp/vox-fake/prompt.log，方便断言「模型到底看到了什么」。

被测内容通过两个文件控制（改完即时生效，无需重启）：
    /tmp/vox-fake/transcript.txt   → ASR 返回的转写文本
    /tmp/vox-fake/answer.txt       → LLM 返回的回答

用法：
    python3 tools/fake-server.py [端口]      # 默认 8787
"""

import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BASE_DIR = "/tmp/vox-fake"
TRANSCRIPT_FILE = os.path.join(BASE_DIR, "transcript.txt")
ANSWER_FILE = os.path.join(BASE_DIR, "answer.txt")
PROMPT_LOG = os.path.join(BASE_DIR, "prompt.log")


def read(path: str, default: str) -> str:
    try:
        with open(path, encoding="utf-8") as f:
            return f.read().strip()
    except OSError:
        return default


def delay() -> float:
    """/tmp/vox-fake/delay 里的秒数 —— 用来模拟「模型想很久」，
    好测试「处理中按热键会不会凭空开新录音」。"""
    try:
        with open(os.path.join(BASE_DIR, "delay"), encoding="utf-8") as f:
            return float(f.read().strip() or 0)
    except (OSError, ValueError):
        return 0.0


def log(text: str) -> None:
    os.makedirs(BASE_DIR, exist_ok=True)
    with open(PROMPT_LOG, "a", encoding="utf-8") as f:
        f.write(text + "\n")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # 静音默认访问日志
        pass

    def _reply(self, payload: bytes) -> None:
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length)
        path = self.path

        if "speech_to_text" in path:
            log(f"=== ASR {path} body={len(body)}B")
            payload = json.dumps(
                {"text": read(TRANSCRIPT_FILE, "测试文本"), "duration": 1.5},
                ensure_ascii=False,
            ).encode("utf-8")
        elif "messages" in path:
            try:
                req = json.loads(body)
                text = req["messages"][0]["content"][0]["text"]
            except Exception:
                text = body.decode("utf-8", "replace")
            log("=== LLM PROMPT ===\n" + text)
            d = delay()
            if d > 0:
                time.sleep(d)
            payload = json.dumps(
                {
                    "content": [
                        {"type": "text", "text": read(ANSWER_FILE, "假答案")}
                    ]
                },
                ensure_ascii=False,
            ).encode("utf-8")
        else:
            log(f"=== 未知路径 {path}")
            payload = b"{}"

        self._reply(payload)


def main() -> int:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
    os.makedirs(BASE_DIR, exist_ok=True)
    if not os.path.exists(TRANSCRIPT_FILE):
        with open(TRANSCRIPT_FILE, "w", encoding="utf-8") as f:
            f.write("测试文本")
    if not os.path.exists(ANSWER_FILE):
        with open(ANSWER_FILE, "w", encoding="utf-8") as f:
            f.write("假答案")
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"fake ASR/LLM listening on http://127.0.0.1:{port}", flush=True)
    srv.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
