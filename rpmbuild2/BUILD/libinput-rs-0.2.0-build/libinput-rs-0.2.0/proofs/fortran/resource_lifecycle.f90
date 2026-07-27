module resource_lifecycle_model
  implicit none
  private

  integer, parameter :: descriptor_absent = 0
  integer, parameter :: descriptor_open = 1
  integer, parameter :: descriptor_closed = 2
  integer, parameter, public :: backend_path = 1
  integer, parameter, public :: backend_udev = 2

  type, public :: restricted_descriptor
    private
    integer :: state = descriptor_absent
    integer :: close_count = 0
  end type restricted_descriptor

  type, public :: hotplug_permission
    private
    logical :: active = .false.
  end type hotplug_permission

  public :: acquire_descriptor, reject_descriptor, remove_descriptor
  public :: descriptor_is_closed, descriptor_closes
  public :: enable_hotplug, disable_hotplug, hotplug_is_active

contains

  logical function acquire_descriptor(descriptor) result(acquired)
    type(restricted_descriptor), intent(inout) :: descriptor
    acquired = descriptor%state /= descriptor_open
    if (acquired) descriptor%state = descriptor_open
  end function acquire_descriptor

  logical function close_descriptor(descriptor) result(closed)
    type(restricted_descriptor), intent(inout) :: descriptor
    closed = descriptor%state == descriptor_open
    if (closed) then
      descriptor%state = descriptor_closed
      descriptor%close_count = descriptor%close_count + 1
    end if
  end function close_descriptor

  logical function reject_descriptor(descriptor) result(closed)
    type(restricted_descriptor), intent(inout) :: descriptor
    closed = close_descriptor(descriptor)
  end function reject_descriptor

  logical function remove_descriptor(descriptor) result(closed)
    type(restricted_descriptor), intent(inout) :: descriptor
    closed = close_descriptor(descriptor)
  end function remove_descriptor

  pure logical function descriptor_is_closed(descriptor) result(closed)
    type(restricted_descriptor), intent(in) :: descriptor
    closed = descriptor%state == descriptor_closed
  end function descriptor_is_closed

  pure integer function descriptor_closes(descriptor) result(count)
    type(restricted_descriptor), intent(in) :: descriptor
    count = descriptor%close_count
  end function descriptor_closes

  logical function enable_hotplug(backend, permission) result(enabled)
    integer, intent(in) :: backend
    type(hotplug_permission), intent(inout) :: permission
    enabled = backend == backend_udev .and. .not. permission%active
    if (enabled) permission%active = .true.
  end function enable_hotplug

  logical function disable_hotplug(permission) result(disabled)
    type(hotplug_permission), intent(inout) :: permission
    disabled = permission%active
    if (disabled) permission%active = .false.
  end function disable_hotplug

  pure logical function hotplug_is_active(permission) result(active)
    type(hotplug_permission), intent(in) :: permission
    active = permission%active
  end function hotplug_is_active

end module resource_lifecycle_model

program verify_resource_lifecycle
  use resource_lifecycle_model
  implicit none

  type(restricted_descriptor) :: descriptor
  type(hotplug_permission) :: permission
  logical :: changed

  changed = acquire_descriptor(descriptor)
  if (.not. changed) error stop "descriptor acquisition failed"
  changed = reject_descriptor(descriptor)
  if (.not. changed) error stop "rejected descriptor was not closed"
  if (.not. descriptor_is_closed(descriptor)) error stop "descriptor remained open"
  changed = remove_descriptor(descriptor)
  if (changed) error stop "descriptor was closed more than once"
  if (descriptor_closes(descriptor) /= 1) error stop "incorrect close count"

  changed = acquire_descriptor(descriptor)
  if (.not. changed) error stop "descriptor reacquisition failed"
  changed = remove_descriptor(descriptor)
  if (.not. changed) error stop "removed descriptor was not closed"
  if (descriptor_closes(descriptor) /= 2) error stop "remove did not close exactly once"

  changed = enable_hotplug(backend_path, permission)
  if (changed) error stop "path backend received hotplug permission"
  if (hotplug_is_active(permission)) error stop "path hotplug permission became active"

  changed = enable_hotplug(backend_udev, permission)
  if (.not. changed) error stop "udev backend did not receive hotplug permission"
  changed = enable_hotplug(backend_udev, permission)
  if (changed) error stop "duplicate hotplug permission was issued"
  changed = disable_hotplug(permission)
  if (.not. changed) error stop "hotplug permission was not consumed"
  changed = disable_hotplug(permission)
  if (changed) error stop "hotplug permission was consumed twice"
end program verify_resource_lifecycle
