module fail_open_model
  implicit none
  private

  type, public :: input_state
    private
    logical :: sink_ready = .false.
    logical :: grabbed = .false.
  end type input_state

  public :: prepare_sink, attempt_grab, release_grab, forwarding_failed
  public :: invariant_holds, is_grabbed

contains

  subroutine prepare_sink(state)
    type(input_state), intent(inout) :: state
    state%sink_ready = .true.
  end subroutine prepare_sink

  logical function attempt_grab(state) result(accepted)
    type(input_state), intent(inout) :: state
    accepted = state%sink_ready .and. .not. state%grabbed
    if (accepted) state%grabbed = .true.
  end function attempt_grab

  subroutine release_grab(state)
    type(input_state), intent(inout) :: state
    state%grabbed = .false.
  end subroutine release_grab

  subroutine forwarding_failed(state)
    type(input_state), intent(inout) :: state
    call release_grab(state)
  end subroutine forwarding_failed

  pure logical function invariant_holds(state) result(valid)
    type(input_state), intent(in) :: state
    valid = .not. state%grabbed .or. state%sink_ready
  end function invariant_holds

  pure logical function is_grabbed(state) result(grabbed)
    type(input_state), intent(in) :: state
    grabbed = state%grabbed
  end function is_grabbed

end module fail_open_model

program verify_fail_open
  use fail_open_model
  implicit none

  type(input_state) :: state
  logical :: accepted

  if (.not. invariant_holds(state)) error stop "invalid initial state"

  accepted = attempt_grab(state)
  if (accepted) error stop "grab accepted without a prepared sink"
  if (is_grabbed(state)) error stop "failed grab changed ownership"

  call prepare_sink(state)
  accepted = attempt_grab(state)
  if (.not. accepted) error stop "grab rejected with a prepared sink"
  if (.not. invariant_holds(state)) error stop "grab violated invariant"

  accepted = attempt_grab(state)
  if (accepted) error stop "duplicate grab accepted"

  call forwarding_failed(state)
  if (is_grabbed(state)) error stop "forwarding failure retained grab"
  if (.not. invariant_holds(state)) error stop "release violated invariant"
end program verify_fail_open
