module restricted_discovery_model
  implicit none
  private

  integer, parameter, public :: scan_missing = 0
  integer, parameter, public :: scan_candidate = 1

  public :: scan_by_direct_open, scan_by_name, restricted_open

contains

  pure integer function scan_by_direct_open(direct_access) result(scan)
    logical, intent(in) :: direct_access

    if (direct_access) then
      scan = scan_candidate
    else
      scan = scan_missing
    end if
  end function scan_by_direct_open

  pure integer function scan_by_name(directory_entry_visible) result(scan)
    logical, intent(in) :: directory_entry_visible

    if (directory_entry_visible) then
      scan = scan_candidate
    else
      scan = scan_missing
    end if
  end function scan_by_name

  pure logical function restricted_open(scan, callback_grants_access) result(opened)
    integer, intent(in) :: scan
    logical, intent(in) :: callback_grants_access

    opened = scan == scan_candidate .and. callback_grants_access
  end function restricted_open

end module restricted_discovery_model

program verify_restricted_discovery
  use restricted_discovery_model
  implicit none

  integer :: legacy_scan
  integer :: replacement_scan

  legacy_scan = scan_by_direct_open(.false.)
  if (legacy_scan /= scan_missing) then
    error stop "direct-open discovery retained a restricted device"
  end if

  replacement_scan = scan_by_name(.true.)
  if (replacement_scan /= scan_candidate) then
    error stop "name-only discovery lost a listed event device"
  end if

  if (.not. restricted_open(replacement_scan, .true.)) then
    error stop "restricted callback did not open a discovered device"
  end if

  if (restricted_open(replacement_scan, .false.)) then
    error stop "restricted callback denial did not fail open"
  end if

  if (scan_by_name(.false.) /= scan_missing) then
    error stop "missing directory entry produced a candidate"
  end if
end program verify_restricted_discovery
