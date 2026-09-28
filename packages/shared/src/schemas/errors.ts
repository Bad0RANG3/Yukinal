import { z } from "zod";

import { ERROR_CATEGORIES } from "../types/errors.js";

/**
 * Runtime gate for the canonical error taxonomy. The UI and the Rust host both
 * parse the same category strings; a hand-copied enum on either side would be the
 * one place a new category could be accepted by a screen but rejected at the IPC
 * boundary.
 */
export const ErrorCategorySchema = z.enum(ERROR_CATEGORIES);
