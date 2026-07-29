module capforge
  use iso_c_binding
  implicit none
  private

  public :: cf_classify, cf_bit_test, cf_parse_hex_words
  public :: cf_knn_scores, cf_tiny_mlp_scores

  integer(c_int), parameter :: cf_unknown = 0
  integer(c_int), parameter :: cf_keyboard = 1
  integer(c_int), parameter :: cf_key = 2
  integer(c_int), parameter :: cf_mouse = 3
  integer(c_int), parameter :: cf_touchpad = 4
  integer(c_int), parameter :: cf_touchscreen = 5
  integer(c_int), parameter :: cf_tablet = 6
  integer(c_int), parameter :: cf_joystick = 7
  integer(c_int), parameter :: cf_switch = 8

  integer(c_int), parameter :: ev_key = 1
  integer(c_int), parameter :: ev_rel = 2
  integer(c_int), parameter :: ev_abs = 3
  integer(c_int), parameter :: ev_sw = 5
  integer(c_int), parameter :: btn_left = int(z'110')
  integer(c_int), parameter :: btn_joystick = int(z'120')
  integer(c_int), parameter :: btn_tool_pen = int(z'140')
  integer(c_int), parameter :: btn_tool_finger = int(z'145')
  integer(c_int), parameter :: btn_touch = int(z'14a')
  integer(c_int), parameter :: abs_x = 0
  integer(c_int), parameter :: abs_y = 1
  integer(c_int), parameter :: abs_mt_slot = int(z'2f')
  integer(c_int), parameter :: abs_mt_position_x = int(z'35')
  integer(c_int), parameter :: input_prop_pointer = 0
  integer(c_int), parameter :: input_prop_direct = 1

contains

  subroutine cf_knn_scores(features, nfeatures, centroids, nprofiles, scores) &
      bind(C, name="cf_knn_scores")
    integer(c_int), value, intent(in) :: nfeatures, nprofiles
    real(c_double), intent(in) :: features(*), centroids(*)
    real(c_double), intent(out) :: scores(*)
    real(c_double) :: delta, distance
    integer :: feature_index, profile_index, offset

    do profile_index = 1, nprofiles
      distance = 0.0_c_double
      offset = (profile_index - 1) * nfeatures
      do feature_index = 1, nfeatures
        delta = features(feature_index) - centroids(offset + feature_index)
        distance = distance + delta * delta
      end do
      scores(profile_index) = -distance
    end do
  end subroutine cf_knn_scores

  subroutine cf_tiny_mlp_scores(features, nfeatures, input_weights, &
      hidden_bias, nhidden, output_weights, output_bias, nprofiles, scores) &
      bind(C, name="cf_tiny_mlp_scores")
    integer(c_int), value, intent(in) :: nfeatures, nhidden, nprofiles
    real(c_double), intent(in) :: features(*), input_weights(*), hidden_bias(*)
    real(c_double), intent(in) :: output_weights(*), output_bias(*)
    real(c_double), intent(out) :: scores(*)
    real(c_double) :: hidden(max(1, nhidden)), value
    integer :: feature_index, hidden_index, profile_index, offset

    do hidden_index = 1, nhidden
      value = hidden_bias(hidden_index)
      offset = (hidden_index - 1) * nfeatures
      do feature_index = 1, nfeatures
        value = value + input_weights(offset + feature_index) * features(feature_index)
      end do
      hidden(hidden_index) = tanh(value)
    end do
    do profile_index = 1, nprofiles
      value = output_bias(profile_index)
      offset = (profile_index - 1) * nhidden
      do hidden_index = 1, nhidden
        value = value + output_weights(offset + hidden_index) * hidden(hidden_index)
      end do
      scores(profile_index) = value
    end do
  end subroutine cf_tiny_mlp_scores

  logical(c_bool) function cf_bit_test(words, nwords, code) &
      bind(C, name="cf_bit_test")
    integer(c_int), value, intent(in) :: nwords, code
    integer(c_int64_t), intent(in) :: words(*)
    integer :: index, offset

    index = code / 64
    offset = mod(code, 64)
    if (index < 0 .or. index >= nwords) then
      cf_bit_test = .false._c_bool
    else
      cf_bit_test = iand(shiftr(words(index + 1), offset), 1_c_int64_t) == 1_c_int64_t
    end if
  end function cf_bit_test

  subroutine cf_parse_hex_words(hexstr, nchars, words, nwords) &
      bind(C, name="cf_parse_hex_words")
    character(c_char), intent(in) :: hexstr(*)
    integer(c_int), value, intent(in) :: nchars, nwords
    integer(c_int64_t), intent(out) :: words(*)
    character(len=:), allocatable :: source
    character(len=32) :: token
    integer(c_int64_t), allocatable :: parsed(:)
    integer(c_int64_t) :: value
    integer :: cursor, first, index, count, status

    do index = 1, nwords
      words(index) = 0_c_int64_t
    end do
    if (nchars <= 0 .or. nwords <= 0) return

    allocate(character(len=nchars) :: source)
    do index = 1, nchars
      source(index:index) = hexstr(index)
    end do
    allocate(parsed(nwords))
    parsed = 0_c_int64_t
    count = 0
    cursor = 1
    do while (cursor <= len(source) .and. count < nwords)
      do while (cursor <= len(source) .and. iachar(source(cursor:cursor)) <= 32)
        cursor = cursor + 1
      end do
      if (cursor > len(source)) exit
      first = cursor
      do while (cursor <= len(source) .and. iachar(source(cursor:cursor)) > 32)
        cursor = cursor + 1
      end do
      token = ''
      token = source(first:cursor - 1)
      read(token, '(Z32)', iostat=status) value
      if (status == 0) then
        count = count + 1
        parsed(count) = value
      end if
    end do
    do index = 1, count
      words(index) = parsed(count - index + 1)
    end do
  end subroutine cf_parse_hex_words

  integer(c_int) function cf_classify(ev, nev, key, nkey, rel, nrel, &
      absolute, nabs, prop, nprop) bind(C, name="cf_classify")
    integer(c_int), value, intent(in) :: nev, nkey, nrel, nabs, nprop
    integer(c_int64_t), intent(in) :: ev(*), key(*), rel(*), absolute(*), prop(*)
    logical :: has_keys, has_relative, has_absolute, has_switch
    logical :: xy, multitouch, finger, touch, pen, left, joystick
    logical :: relative_xy, direct, pointer
    integer :: code, key_count

    has_keys = cf_bit_test(ev, nev, ev_key)
    has_relative = cf_bit_test(ev, nev, ev_rel)
    has_absolute = cf_bit_test(ev, nev, ev_abs)
    has_switch = cf_bit_test(ev, nev, ev_sw)
    xy = has_absolute .and. cf_bit_test(absolute, nabs, abs_x) .and. &
      cf_bit_test(absolute, nabs, abs_y)
    multitouch = cf_bit_test(absolute, nabs, abs_mt_slot) .or. &
      cf_bit_test(absolute, nabs, abs_mt_position_x)
    finger = cf_bit_test(key, nkey, btn_tool_finger)
    touch = cf_bit_test(key, nkey, btn_touch)
    pen = cf_bit_test(key, nkey, btn_tool_pen)
    left = cf_bit_test(key, nkey, btn_left)
    joystick = cf_bit_test(key, nkey, btn_joystick)
    relative_xy = has_relative .and. (cf_bit_test(rel, nrel, 0) .or. &
      cf_bit_test(rel, nrel, 1))
    direct = cf_bit_test(prop, nprop, input_prop_direct)
    pointer = cf_bit_test(prop, nprop, input_prop_pointer)

    if (pen .and. xy) then
      cf_classify = cf_tablet
    else if ((finger .or. (touch .and. pointer .and. .not. direct)) .and. xy) then
      cf_classify = cf_touchpad
    else if ((direct .or. (touch .and. multitouch)) .and. xy) then
      cf_classify = cf_touchscreen
    else if (relative_xy .and. left) then
      cf_classify = cf_mouse
    else if (joystick .and. has_absolute) then
      cf_classify = cf_joystick
    else if (has_keys) then
      key_count = 0
      do code = 1, 254
        if (cf_bit_test(key, nkey, code)) key_count = key_count + 1
      end do
      if (key_count > 20) then
        cf_classify = cf_keyboard
      else if (key_count > 0) then
        cf_classify = cf_key
      else
        cf_classify = cf_unknown
      end if
    else if (has_switch) then
      cf_classify = cf_switch
    else
      cf_classify = cf_unknown
    end if
  end function cf_classify

end module capforge
