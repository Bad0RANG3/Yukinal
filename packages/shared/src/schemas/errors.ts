import { z } from "zod";

import { ERROR_CATEGORIES } from "../types/errors.js";

/**
 * Runtime gate for the canonical error taxonomy. The UI and the host both handle the same
 * category strings; a hand-copied enum on either side would be the one place a new category
 * could be accepted by a screen but rejected at the IPC boundary. The host side is a
 * literal comparison, not a generated enum (`native/desktop_host/src/ipc_router.cpp:5473`),
 * so adopting a new category is a two-file change with only this file's half gated.
 */
export const ErrorCategorySchema = z.enum(ERROR_CATEGORIES);
