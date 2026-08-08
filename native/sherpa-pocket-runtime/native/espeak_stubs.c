// SPDX-License-Identifier: Apache-2.0
//
// Pocket TTS has no phonemizer and never calls these functions. They satisfy
// three references retained by sherpa-onnx's generic TTS factory without
// incorporating the unrelated GPL eSpeak NG implementation. Any accidental
// non-Pocket path fails closed.

#include <stddef.h>

int espeak_Initialize(int output, int buffer_length, const char *path,
                      int options) {
  (void)output;
  (void)buffer_length;
  (void)path;
  (void)options;
  return -1;
}

int espeak_SetVoiceByName(const char *name) {
  (void)name;
  return 2;
}

const char *espeak_TextToPhonemesWithTerminator(const void **text,
                                                 int text_mode,
                                                 int phoneme_mode,
                                                 int *terminator) {
  (void)text_mode;
  (void)phoneme_mode;
  if (text != NULL) {
    *text = NULL;
  }
  if (terminator != NULL) {
    *terminator = 0;
  }
  return NULL;
}
