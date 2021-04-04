(ns-push 'shell-read)

(defn callable? (com)
    ; Want the actual thing pointed to by the symbol in com for the test.
    (set! com (shell-read::find-symbol com))
    (if (def? (ref com))
        (do (set! com (eval (sym com)))
            (or (builtin? com) (lambda? com) (macro? com)))
        nil))

(defmacro sys-apply (com &rest args)
    (if (callable? com)
        `(,com ,@args)
        `(syscall ,com ,@args)))

(defn find-symbol (com)
	(var val (sym *active-ns* "::" com))
	(if (def? (ref val)) val (sym "root::" com)))

(defmacro var-or-env (key)
    (let ((key-new (find-symbol key)))
        (if (def? (ref key-new))
            `,key
            `(get-env ,key))))

(let ((paren-level 0))

(defn shell-read-int (stream in-paren)
    (let ((in-quote nil)
          (token (str ""))
          (new-pair (list))
          (last-pair (list))
          (result (list))
          (close-token)
          (add-exp)
          (do-read)
          (maybe-glob?)
          (first-tok #t)
          (first-sym #t)
          (just-read nil)
          (var-bracket nil)
          (done nil))
        (set! add-exp (fn (exp)
            (set! new-pair (join exp nil))
            (if (nil? last-pair) (set! result new-pair))
            (xdr! last-pair new-pair)
            (set! last-pair new-pair)))
        (set! maybe-glob? (fn (token)
              (or (str-contains "*" token)
                  (str-contains "?" token)
                  (str-contains "[" token)
                  (str-contains "{" token))))
        (set! close-token (fn ()
            (if (not (str-empty? token))
                (if first-sym (do (add-exp (sym token)) (set! first-sym nil))
                    (maybe-glob? token) (add-exp (list 'glob token))
                    (add-exp token)))
            (set! token (str ""))))
        (set! do-read (fn (ch)
                    (if (not (char-whitespace? ch))
                        (str-push! token ch))
                    (close-token)
                    (add-exp (read stream))
                    (set! just-read #t)))
        (if in-paren (add-exp (sym "shell-read::sys-apply")))
        ((fn (last-ch ch peek-ch)
            (cond
                ((and (= ch #\() first-tok (= peek-ch #\())
                    (set! result (read stream))
                    (set! ch (str-iter-next! stream))
                    ((fn () (if (and (char? ch)(char-whitespace? ch)) (do (set! ch (str-iter-next! stream))(recur)))))
                    (if (not (= #\) ch))
                        (err "Unbalanced ) in '\$' shell read macro"))
                    (set! done #t))
                ((and (= ch #\() first-tok)
                    (set! paren-level (+ paren-level 1))
                    (add-exp (sym "shell-read::sys-apply"))
                    (set! in-paren #t))
                ((and (= ch #\{) first-tok)
                    (set! var-bracket #t))
                ((and (not (= last-ch #\\))(= ch #\)) (> paren-level 0))
                    (set! paren-level (- paren-level 1))
                    (set! done #t))
                ((and (not (= ch #\\))(= peek-ch #\))(not in-paren))
                    (if (not (= ch #\}))(str-push! token ch))
                    (set! done #t))
                ((and (char-whitespace? ch)(not in-paren)(not var-bracket))
                    (set! done #t))
                ((and (= ch #\}) var-bracket)
                    (set! done #t))
                ((and (or (and (char? peek-ch)(char-whitespace? peek-ch))(= peek-ch #\"))(not in-paren)(not var-bracket))
                    (str-push! token ch)
                    (set! done #t))
                ((and (not (= ch #\\))(or (= peek-ch #\")(= peek-ch #\$)))
                    (do-read ch))
                ((char-whitespace? ch)
                    (close-token))
                ((str-push! token ch) nil))
            (set! first-tok nil)
            (if (and (not done)(not (str-iter-empty? stream)))
                (if just-read
                    (do (set! just-read nil)(recur #\  #\  (str-iter-peek stream)))
                    (recur ch (str-iter-next! stream)(str-iter-peek stream)))
                (if (not (str-empty? token))
                    (if in-paren
                        (close-token)
                        (do (add-exp (sym "shell-read::var-or-env"))(add-exp (sym token))))))
             )#\ (str-iter-next! stream)(str-iter-peek stream))
        result)))

(defn shell-read (stream ch_start) (shell-read::shell-read-int stream nil))

(def *ns-exports* nil)

(ns-pop)

(hash-set! *read-table* #\$ 'shell-read::shell-read)
(hash-set! *string-read-table* #\$ 'shell-read::shell-read)
