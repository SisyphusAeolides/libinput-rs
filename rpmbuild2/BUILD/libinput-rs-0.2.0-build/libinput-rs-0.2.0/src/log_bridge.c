#include <stdarg.h>

typedef void (*input_log_handler)(void *context,
                                  int priority,
                                  const char *format,
                                  va_list arguments);

__attribute__((visibility("hidden"))) void
input_emit_log(void *handler_pointer,
               void *context,
               int priority,
               const char *format,
               ...)
{
        input_log_handler handler = (input_log_handler)handler_pointer;
        va_list arguments;

        va_start(arguments, format);
        handler(context, priority, format, arguments);
        va_end(arguments);
}
