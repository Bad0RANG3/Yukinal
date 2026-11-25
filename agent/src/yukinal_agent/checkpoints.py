from __future__ import annotations

import os
import stat
from contextlib import asynccontextmanager
from pathlib import Path
from typing import AsyncIterator

_PRIVATE_FILE = 0o600
_PRIVATE_DIR = 0o700


def _require_regular_file(path: Path, message: str) -> None:
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError(message)


def _require_directory(path: Path, message: str) -> None:
    if not stat.S_ISDIR(path.lstat().st_mode):
        raise ValueError(message)


def _windows_owner_only_dacl(path: Path, *, directory: bool) -> None:
    """Rewrite the DACL so that only the process token owner can reach ``path``.

    ``os.chmod`` on Windows only toggles the read-only flag, so a file created
    inside the per-user data directory inherits whatever ACL that directory
    carries and stays readable by every local account. Checkpoints hold prompts
    and tool output, so the DACL is set explicitly instead of assumed.
    """
    import ctypes
    from ctypes import wintypes

    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    kernel32.GetCurrentProcess.argtypes = []
    kernel32.CloseHandle.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.LocalFree.restype = ctypes.c_void_p
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    advapi32.OpenProcessToken.restype = wintypes.BOOL
    advapi32.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    advapi32.GetTokenInformation.restype = wintypes.BOOL
    advapi32.GetTokenInformation.argtypes = [
        wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD),
    ]
    advapi32.SetEntriesInAclW.restype = wintypes.DWORD
    advapi32.SetNamedSecurityInfoW.restype = wintypes.DWORD
    advapi32.SetNamedSecurityInfoW.argtypes = [
        wintypes.LPWSTR, ctypes.c_int, wintypes.DWORD,
        ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p,
    ]

    token_query = 0x0008
    token_user_class = 1
    se_file_object = 1
    dacl_security_information = 0x00000004
    protected_dacl_security_information = 0x80000000
    set_access = 2
    sub_containers_and_objects_inherit = 0x03
    trustee_is_sid = 0
    trustee_is_user = 1
    file_all_access = 0x001F01FF
    error_success = 0

    class SID_AND_ATTRIBUTES(ctypes.Structure):
        _fields_ = [("Sid", ctypes.c_void_p), ("Attributes", wintypes.DWORD)]

    class TOKEN_USER(ctypes.Structure):
        _fields_ = [("User", SID_AND_ATTRIBUTES)]

    class TRUSTEE_W(ctypes.Structure):
        _fields_ = [
            ("pMultipleTrustee", ctypes.c_void_p),
            ("MultipleTrusteeOperation", ctypes.c_int),
            ("TrusteeForm", ctypes.c_int),
            ("TrusteeType", ctypes.c_int),
            ("ptstrName", ctypes.c_void_p),
        ]

    class EXPLICIT_ACCESS_W(ctypes.Structure):
        _fields_ = [
            ("grfAccessPermissions", wintypes.DWORD),
            ("grfAccessMode", ctypes.c_int),
            ("grfInheritance", wintypes.DWORD),
            ("Trustee", TRUSTEE_W),
        ]

    token = wintypes.HANDLE()
    if not advapi32.OpenProcessToken(kernel32.GetCurrentProcess(), token_query, ctypes.byref(token)):
        raise OSError(ctypes.get_last_error(), "cannot open the process token to protect agent state")
    try:
        needed = wintypes.DWORD(0)
        advapi32.GetTokenInformation(token, token_user_class, None, 0, ctypes.byref(needed))
        if needed.value <= 0:
            raise OSError(ctypes.get_last_error(), "cannot size the process token user")
        buffer = ctypes.create_string_buffer(needed.value)
        if not advapi32.GetTokenInformation(token, token_user_class, buffer, needed, ctypes.byref(needed)):
            raise OSError(ctypes.get_last_error(), "cannot read the process token user")
        sid = ctypes.cast(buffer, ctypes.POINTER(TOKEN_USER)).contents.User.Sid

        entry = EXPLICIT_ACCESS_W()
        entry.grfAccessPermissions = file_all_access
        entry.grfAccessMode = set_access
        entry.grfInheritance = sub_containers_and_objects_inherit if directory else 0
        entry.Trustee.TrusteeForm = trustee_is_sid
        entry.Trustee.TrusteeType = trustee_is_user
        entry.Trustee.ptstrName = ctypes.cast(sid, ctypes.c_void_p)

        acl = ctypes.c_void_p()
        if advapi32.SetEntriesInAclW(1, ctypes.byref(entry), None, ctypes.byref(acl)) != error_success:
            raise OSError(ctypes.get_last_error(), "cannot build the agent state DACL")
        try:
            information = dacl_security_information | protected_dacl_security_information
            result = advapi32.SetNamedSecurityInfoW(
                str(path), se_file_object, information, None, None, acl, None
            )
            if result != error_success:
                raise OSError(result, "cannot apply the agent state DACL")
        finally:
            kernel32.LocalFree(acl)
    finally:
        kernel32.CloseHandle(token)


def _restrict_access(path: Path, *, directory: bool) -> None:
    """Grant the current user and nothing else. Raises instead of degrading."""
    if os.name == "nt":
        _windows_owner_only_dacl(path, directory=directory)
        return
    path.chmod(_PRIVATE_DIR if directory else _PRIVATE_FILE)


def _ensure_private_directory(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True, mode=_PRIVATE_DIR)
    _require_directory(path, "Agent state path must be a directory")
    _restrict_access(path, directory=True)


def _ensure_private_file(path: Path, message: str) -> None:
    flags = os.O_CREAT | os.O_EXCL | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    try:
        descriptor = os.open(path, flags, _PRIVATE_FILE)
    except FileExistsError:
        pass
    else:
        os.close(descriptor)
    _require_regular_file(path, message)
    _restrict_access(path, directory=False)


def checkpoint_binding_key(data_dir: str | Path) -> bytes:
    """Private persistent HMAC key; a checkpoint never stores password digests."""
    if not str(data_dir).strip():
        raise ValueError("checkpoint connection binding requires a private data directory")
    root = Path(data_dir).expanduser().resolve() / "agent-state"
    _ensure_private_directory(root)
    path = root / "connection-binding.key"
    flags = os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    try:
        fd = os.open(path, flags, _PRIVATE_FILE)
    except FileExistsError:
        pass
    else:
        try:
            os.write(fd, os.urandom(32))
        finally:
            os.close(fd)
    _require_regular_file(path, "checkpoint binding key must be a regular file")
    _restrict_access(path, directory=False)
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
    try:
        value = os.read(fd, 33)
    finally:
        os.close(fd)
    if len(value) != 32:
        raise ValueError("checkpoint binding key is invalid")
    return value


@asynccontextmanager
async def open_checkpoint_store(data_dir: str | Path) -> AsyncIterator[object]:
    """Open the per-user SQLite checkpoint store with strict deserialization."""
    if not str(data_dir).strip():
        raise ValueError("a private per-user data directory is required for checkpoints")
    state_dir = Path(data_dir).expanduser().resolve() / "agent-state"
    _ensure_private_directory(state_dir)

    database = state_dir / "langgraph-checkpoints.sqlite"
    _ensure_private_file(database, "Agent checkpoint path must be a regular file")

    # Checkpoints contain prompts and tool output. The SQLite saver serializes
    # LangGraph messages with msgpack, so never allow arbitrary class loading
    # from a modified on-disk checkpoint.
    #
    # `LANGGRAPH_STRICT_MSGPACK` cannot be trusted here: langgraph evaluates it
    # once, at import of `langgraph.checkpoint.serde._msgpack`, and this module
    # is imported long after the agent already pulled langgraph in. Passing the
    # serializer explicitly is the only ordering-independent way to stay strict.
    # The environment variable is still set as a second line of defence for any
    # launcher that imports langgraph lazily; correctness does not depend on it.
    os.environ["LANGGRAPH_STRICT_MSGPACK"] = "true"

    import aiosqlite
    from langgraph.checkpoint.serde.jsonplus import JsonPlusSerializer
    from langgraph.checkpoint.sqlite.aio import AsyncSqliteSaver

    serde = JsonPlusSerializer(allowed_msgpack_modules=None)
    async with aiosqlite.connect(str(database)) as connection:
        saver = AsyncSqliteSaver(connection, serde=serde)
        # Create the schema eagerly so a broken store fails at startup instead of
        # in the middle of a run that has already touched a remote host.
        await saver.setup()
        _restrict_access(database, directory=False)
        yield saver
