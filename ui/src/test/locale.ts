import { setLocale } from "../i18n";

/**
 * The tests run in Chinese. This module is imported first by `setup.ts`, before
 * the test file and the application modules are loaded, so the labels that a
 * module builds once (tables of choices, column titles) are built in Chinese
 * too, exactly as they would be on a Chinese system.
 */
setLocale("zh-CN");
