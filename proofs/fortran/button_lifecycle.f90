module button_lifecycle_model
  implicit none
  private

  integer, parameter, public :: state_up = 0
  integer, parameter, public :: state_down = 1
  integer, parameter, public :: input_press = 1
  integer, parameter, public :: input_release = 2
  integer, parameter, public :: input_disconnect = 3
  integer, parameter, public :: output_silent = 0
  integer, parameter, public :: output_pressed = 1
  integer, parameter, public :: output_released = 2

  public :: step

contains

  pure subroutine step(state, input, next_state, output)
    integer, intent(in) :: state, input
    integer, intent(out) :: next_state, output

    next_state = state
    output = output_silent
    select case (input)
    case (input_press)
      if (state == state_up) then
        next_state = state_down
        output = output_pressed
      end if
    case (input_release, input_disconnect)
      if (state == state_down) then
        next_state = state_up
        output = output_released
      end if
    case default
      error stop "invalid button input"
    end select
  end subroutine step

end module button_lifecycle_model

program verify_button_lifecycle
  use button_lifecycle_model
  implicit none

  integer :: first, second, third

  do first = input_press, input_disconnect
    do second = input_press, input_disconnect
      do third = input_press, input_disconnect
        call verify_trace([first, second, third])
      end do
    end do
  end do

contains

  subroutine verify_trace(inputs)
    integer, intent(in) :: inputs(:)
    integer :: state, next_state, output, index
    integer :: presses, releases

    state = state_up
    presses = 0
    releases = 0
    do index = 1, size(inputs)
      call step(state, inputs(index), next_state, output)
      if (output == output_pressed) presses = presses + 1
      if (output == output_released) releases = releases + 1
      if (releases > presses) error stop "release without logical press"
      if (presses - releases > 1) error stop "duplicate logical press"
      if ((next_state == state_down) .neqv. (presses == releases + 1)) then
        error stop "state and output history disagree"
      end if
      state = next_state
    end do

    call step(state, input_disconnect, next_state, output)
    if (next_state /= state_up) error stop "disconnect left button held"
    if (state == state_down .and. output /= output_released) then
      error stop "disconnect failed to balance press"
    end if
  end subroutine verify_trace

end program verify_button_lifecycle
