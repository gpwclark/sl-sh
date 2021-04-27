(ns-push 'shell-read)

(defn find-symbol (com)
  (var val (sym *active-ns* "::" com))
  (if (def? (ref val)) val (sym "root::" com)))

(defn callable? (com)
  ; Want the actual thing pointed to by the symbol in com for the test.
  (set! com (shell-read::find-symbol com))
  (if (def? (ref com))
      (do (set! com (eval (sym com)))
          (or (builtin? com) (lambda? com) (macro? com)))
      nil))

(defn flatten-args (vars-vec from)
  (if (list? from)
      ((fn (data)
           (if (pair? data)
               (do (flatten-args vars-vec (car data))
                   (recur (cdr data)))))
       from)
      (and (vec? from)(> (length from) 0))
      ((fn (i arg args-max)
           (flatten-args vars-vec arg)
           (if (< i args-max)
               (recur (+ i 1)(vec-nth from (+ i 1))args-max)))
       0 (vec-nth from 0) (- (length from) 1))
      (vec? from) nil
      (vec-push! vars-vec (str from))))

(defn fncall (com &rest args)
  (let ((new-args (vec)))
    (flatten-args new-args args)
    (ns-push *active-ns*)
    (unwind-protect
         (if (macro? com) (eval (expand-macro-all `(,com ,@new-args)))
             (apply com new-args))
      (ns-pop))))

; sys-apply needs to be able to handle no args to make the shell reader simpler.
(defmacro sys-apply (&rest args)
  (let ((first-arg (vec-nth args 0))
        (args-len (length args)))
    (if
     (= args-len 0) nil
     (and (= args-len 1)(or (vec? first-arg)(pair? first-arg))) first-arg
     (callable? first-arg) `(shell-read::fncall ,first-arg ,@(vec-slice args 1))
     #t `(syscall ,first-arg ,@(vec-slice args 1)))))

(defmacro var-or-env (key)
    `(if (def? ,key)
        ,key
        (get-env ,key)))

;; This eleminates the trailing (shell-read::sys-apply) that will be on a
;; run-bg-first call if the & was at the end.  Keeps the other endfix code
;; simple and makes sure the $(... &) returns the process object not nil.
(defn run-bg-prep-args (args)
  (let ((args-len (length args)))
    (if (> args-len 0)
        (if (<= (length (vec-nth args (- args-len 1))) 1)
            (vec-slice args 0 (- args-len 1))
            args)
        nil)))

(defmacro run-bg-first (com &rest args)
  `(do (fork ,com) ,@(run-bg-prep-args args)))

(defmacro redir> (exp file) `(out> ,file ,exp))
(defmacro redir>> (exp file) `(out>> ,file ,exp))
(defmacro redir2> (exp file) `(err> ,file ,exp))
(defmacro redir2>> (exp file) `(err>> ,file ,exp))
(defmacro redir&> (exp file) `(out-err> ,file ,exp))
(defmacro redir&>> (exp file) `(out-err>> ,file ,exp))

(defn handle-process (cmd-proc)
	(if (process? cmd-proc) (= 0 (wait cmd-proc)) (not (not cmd-proc))))

(defmacro proc-wait ()
	(fn (cmd) `(handle-process ,cmd)))

(defn consume-whitespace (stream)
  (let ((ch (str-iter-peek stream)))
    (if (and (char? ch)(char-whitespace? ch)) (do (str-iter-next! stream)(consume-whitespace stream)))))

(defn read-string (stream last-ch token first quoted)
  (consume-whitespace stream)
  (let ((done)
        (ch (str-iter-next! stream))
        (peek-ch (str-iter-peek stream)))
    (cond
      ((and first (= ch #\"))
       (set! quoted #t))
      ((and (char-whitespace? ch)(not (= last-ch #\\)(not quoted)))
       (set! done #t))
      ((and (= ch #\")quoted)
       (set! done #t))
      ((and (not (= ch #\\))(or (= peek-ch #\))
                                (= peek-ch #\$)
                                (= peek-ch #\space)
                                (and (= peek-ch #\")(not quoted))))
       (str-push! token ch)
       (set! done #t))
      ((str-push! token ch) nil))
    (if (and (not done)(not (str-iter-empty? stream)))
        (read-string stream ch token nil quoted) ; recur
        token)))

(defn read-var (stream last-ch ch peek-ch var-bracket add-exp token)
  (let ((done))
    (cond
      ((and (char-whitespace? ch)(not var-bracket))
       (set! done #t))
      ((and (not (= ch #\\))(or (= peek-ch #\))
                                (= peek-ch #\$)
                                (= peek-ch #\:)
                                (= peek-ch #\space)
                                (= peek-ch #\"))
            (not var-bracket))
       (str-push! token ch)
       (set! done #t))
      ((and (= ch #\}) var-bracket)
       (set! done #t))
      ((str-push! token ch) nil))
    (if (and (not done)(not (str-iter-empty? stream)))
        (read-var stream ch (str-iter-next! stream)(str-iter-peek stream) var-bracket add-exp token) ;recur
        (do
         (if (str-empty? token) (err "Syntax error, floating '$'."))
         (add-exp (sym "shell-read::var-or-env"))
          (add-exp (sym token))))))

(defn maybe-glob? (token)
  (or (str-contains "*" token)
      (str-contains "?" token)
      (str-contains "[" token)))

(defn get-home ()
  (let ((home (get-env "HOME")))
    (let ((last-idx (- (length home) 1)))
      (if (= #\/ (str-nth last-idx home)) (str-sub 0 last-idx home)
          home))))

(defn expand-tilde (token first-only)
  (let ((home (get-home)))
    (if (str-starts-with "~" token)
        (set! token (str home (str-sub 1 (- (length token) 1) token))))
    (if (not first-only) (do
                          (set! token (str-replace token ":~" (str ":" home)))
                          (set! token (str-replace token "\\~" "~")))))
  token)

;; If we have a token with embedded $ then break it up and wrap in a str.
(defn expand-dollar (token first)
  (if (str-contains #\$ token)
      (let ((toks (vec))
            (new-token (str)))
        (str-iter-start token)
        (if (= (str-iter-peek token) #\$) (vec-push! toks (read token)))
        ((fn (last-ch ch peek-ch done)
             (cond
               ((str-iter-empty? token)
                (if (char? ch) (str-push! new-token ch))
                (if (not (str-empty? new-token))(vec-push! toks new-token))
                (set! done #t))
               ((and (= last-ch #\\)(= ch #\$))
                (str-push! new-token ch))
               ((= ch #\\)) ; skip \ for now.
               ((and (not (= ch #\\))(= peek-ch #\$))
                (str-push! new-token ch)
                (vec-push! toks new-token)
                (set! new-token (str))
                (vec-push! toks (read token)))
               (#t
                (if (= last-ch #\\) (str-push! new-token last-ch))
                (str-push! new-token ch)))
            (if (not done) (recur ch (str-iter-next! token)(str-iter-peek token)done)
                (and (= (length toks) 1)first) (vec-nth toks 0)
                (apply list (sym "str") toks)))
         #\space(str-iter-next! token)(str-iter-peek token)nil))
      token))

(let ((paren-level 0))

  (defn read-list (last-ch ch peek-ch add-exp close-token do-read push-token
                           get-result clear-result)

    (defn setup-chainer (outer-form wrapper last-file)
      (let ((temp-result))
        (close-token)
        (set! temp-result (get-result))
        (clear-result)
        (add-exp (sym outer-form))
        (if wrapper
            (add-exp (list (sym wrapper) temp-result))
            (add-exp temp-result))
        (set! temp-result
              (if last-file
                  (read-string stream #\space (str) #t nil)
                  (if (and (= peek-ch #\$)(= peek-ch #\"))
                      (shell-read-int stream nil)
                      (shell-read-int stream #t))))
        (if wrapper
            (add-exp (list (sym wrapper) temp-result))
            (add-exp temp-result))))

    (let ((just-read)
          (done))
      (cond
        ((and (not (= last-ch #\\))(= ch #\)) (> paren-level 0))
         (set! paren-level (- paren-level 1))
         (set! done #t))
        ((and (not (= last-ch #\\))(= ch #\)))
         (set! done #t))
        ((and (= ch #\$)(= peek-ch #\())
         (push-token ch)
         ((fn (ch plevel)
              (if (not (char? ch)) (err "Missing ')'")
                  (= ch #\() (set! plevel (+ plevel 1))
                  (= ch #\)) (set! plevel (- plevel 1)))
              (push-token ch)
              (if (> plevel 0) (recur (str-iter-next! stream) plevel)))
          (str-iter-next! stream) 0))
        ((and (not (= ch #\\))(= peek-ch #\"))
         (do-read stream ch)
         (set! just-read #t))
        ((and (not (= last-ch #\\))(= ch #\&)(= peek-ch #\&)) ; AND
         (str-iter-next! stream)
         (setup-chainer "and" "shell-read::handle-process" nil)
         (set! done #t))
        ((and (not (= last-ch #\\))(= ch #\|)(= peek-ch #\|)) ; OR
         (str-iter-next! stream)
         (setup-chainer "or" "shell-read::handle-process" nil)
         (set! done #t))
        ((and (not (= last-ch #\\))(= ch #\@)(= peek-ch #\@)) ; DO
         (str-iter-next! stream)
         (setup-chainer "do" nil nil)
         (set! done #t))
        ((and (not (= last-ch #\\))(= ch #\|)) ; PIPE
         (setup-chainer "root::pipe" nil nil)
         (set! done #t))
        ((and (not (= last-ch #\\))(= ch #\>)(= peek-ch #\>)) ; out>>
         (str-iter-next! stream)
         (setup-chainer "shell-read::redir>>" nil #t))
        ((and (not (= last-ch #\\))(= ch #\>)) ; out>
         (setup-chainer "shell-read::redir>" nil #t))
        ((and (not (= last-ch #\\))(= ch #\&)(= peek-ch #\>)) ; out-err>(>)
         (str-iter-next! stream)
         (if (= (str-iter-peek stream) #\>)
             (do (str-iter-next! stream)
                 (setup-chainer "shell-read::redir&>>" nil #t))
             (setup-chainer "shell-read::redir&>" nil #t)))
        ((and (not (= last-ch #\\))(= ch #\2)(= peek-ch #\>)) ; err>(>)
         (str-iter-next! stream)
         (if (= (str-iter-peek stream) #\>)
             (do (str-iter-next! stream)
                 (setup-chainer "shell-read::redir2>>" nil #t))
             (setup-chainer "shell-read::redir2>" nil #t)))
        ((and (not (= last-ch #\\))(= ch #\&)) ; Background
         (setup-chainer "shell-read::run-bg-first" nil nil)
         (set! done #t))
        ((or (not (char? ch))(char-whitespace? ch))
         (close-token))
        ((push-token ch) nil))
      (if (and (not done)(not (str-iter-empty? stream)))
          (if just-read
              (do
               (set! just-read nil)
               (read-list #\space #\space (str-iter-peek stream) add-exp
                close-token do-read push-token get-result clear-result)) ;recur
              (read-list ch (str-iter-next! stream)(str-iter-peek stream)
                         add-exp close-token do-read push-token get-result
                         clear-result)) ;recur
          (close-token))))

  (defn shell-read-int (stream in-paren)
    (let ((result)
          (token)
          (var-bracket nil)
          (last-pair (list))
          (add-exp)
          (close-token)
          (push-token)
          (do-read)
          (get-result)
          (clear-result)
          (first-sym #t)
          (ch)
          (peek-ch))

      (set! token (str))
      (set! add-exp (fn (exp)
                        (let ((new-pair (join exp nil)))
                          (if (nil? last-pair) (set! result new-pair))
                          (xdr! last-pair new-pair)
                          (set! last-pair new-pair))))

      (set! close-token (fn ()
                            (if (not (str-empty? token))
                                (if first-sym (let ((tng (expand-dollar (expand-tilde token #t) #t)))
                                                (if (string? tng) (add-exp (sym tng))
                                                    ; XXX TODO- if this is a (str...) list then deal with that.
                                                    (add-exp tng))
                                                (set! first-sym nil))
                                    (maybe-glob? token) (add-exp (list 'glob (expand-dollar (expand-tilde token) nil)))
                                    (str-contains "~" token) (add-exp (expand-dollar (expand-tilde token nil) nil))
                                    (add-exp (expand-dollar token nil))))
                            (set! token (str))))

      (set! push-token (fn (ch) (str-push! token ch)))

      (set! do-read (fn (stream ch)
                        (if (not (char-whitespace? ch))
                            (str-push! token ch))
                        (close-token)
                        (add-exp (read stream))))

      (set! get-result (fn () result))
      (set! clear-result (fn () (set! last-pair (list))(set! result nil)))

      (set! ch (str-iter-next! stream))
      (set! peek-ch (str-iter-peek stream))
      (cond
        (in-paren
         (add-exp (sym "shell-read::sys-apply"))
         (read-list #\space ch peek-ch add-exp close-token do-read push-token
                    get-result clear-result))
        ((and (= ch #\()(= peek-ch #\())
         (set! result (read stream))
         (consume-whitespace stream)
         (set! ch (str-iter-next! stream))
         (if (not (= #\) ch))
             (err (str "Unbalanced ) in '\$' shell read macro, got " ch)))
         result)
        ((= ch #\()
         (set! paren-level (+ paren-level 1))
         (add-exp (sym "shell-read::sys-apply"))
         (read-list #\space (str-iter-next! stream)(str-iter-peek stream)
                    add-exp close-token do-read push-token get-result
                    clear-result))
        ((= ch #\{)
         (read-var stream #\space (str-iter-next! stream)(str-iter-peek stream) #t add-exp token))
        (#t
         (read-var stream #\space ch peek-ch nil add-exp token)))
      result)))

(defn shell-read (stream ch_start) (shell-read::shell-read-int stream nil))

(def *ns-exports* nil)

(ns-pop)

(hash-set! *read-table* #\$ 'shell-read::shell-read)
(hash-set! *string-read-table* #\$ 'shell-read::shell-read)
