#include <errno.h>
#include <libinput.h>

static int open_restricted(const char *path, int flags, void *user_data)
{
    (void)path;
    (void)flags;
    (void)user_data;
    return -ENODEV;
}

static void close_restricted(int fd, void *user_data)
{
    (void)fd;
    (void)user_data;
}

static const struct libinput_interface interface = {
    .open_restricted = open_restricted,
    .close_restricted = close_restricted,
};

int main(void)
{
    struct libinput *libinput = libinput_path_create_context(&interface, NULL);
    if (libinput == NULL)
        return 1;

    const int dispatch_status = libinput_dispatch(libinput);
    libinput_unref(libinput);
    return dispatch_status == 0 ? 0 : 2;
}
